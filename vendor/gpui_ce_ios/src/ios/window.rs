//! iOS Window implementation using UIWindow and UIViewController.
//!
//! iOS windows are fundamentally different from desktop windows:
//! - Always fullscreen (or split-screen on iPad)
//! - No title bar or window chrome
//! - Touch-based input
//! - Safe area insets for notch/home indicator
//!
//! The window is backed by a UIWindow containing a UIViewController
//! whose view hosts a CAMetalLayer.

use super::events::*;
use super::text_input::TextInputView;
use super::{CallbackSlot, IosDisplay, platform::IosPlatformState};
use accesskit_ios::SubclassingAdapter;
use gpui::accesskit::{ActionHandler, ActivationHandler, DeactivationHandler};
use gpui::{
    A11yCallbacks, AnyWindowHandle, Bounds, Capslock, DevicePixels, DispatchEventResult, DisplayId,
    Edges, GpuSpecs, Modifiers, Pixels, PlatformAtlas, PlatformDisplay, PlatformInput,
    PlatformInputHandler, PlatformWindow, Point, PromptButton, PromptLevel, RequestFrameOptions,
    Scene, Size, TextInputConfiguration, TextInputStateChange, TouchEvent, TouchId, TouchPhase,
    WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowInsets,
    WindowParams, WindowVisibility, px, size,
};
use gpui_apple::metal_renderer::MetalRenderer;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, ProtocolObject};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    NSNotification, NSNotificationCenter, NSNumber, NSObjectProtocol, NSSet, NSValue,
};
use objc2_quartz_core::CAMetalLayer;
use objc2_ui_kit::{
    NSValueUIGeometryExtensions, UIKeyboardAnimationDurationUserInfoKey,
    UIKeyboardFrameEndUserInfoKey, UIKeyboardWillChangeFrameNotification,
    UIKeyboardWillHideNotification, UIStatusBarStyle, UITextInteraction, UITextInteractionDelegate,
    UITextInteractionMode,
};
use objc2_ui_kit::{
    UICoordinateSpace, UIEvent, UIScreen, UITouch, UITraitCollection, UITraitEnvironment,
    UIUserInterfaceStyle, UIView, UIViewAutoresizing, UIViewController, UIWindow, UIWindowScene,
};
use parking_lot::Mutex;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, UiKitDisplayHandle, UiKitWindowHandle};
use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    ptr::NonNull,
    rc::{Rc, Weak},
    sync::Arc,
    time::{Duration, Instant},
};

const KEYBOARD_DISMISS_DISTANCE: Pixels = px(24.);
struct IosA11yActivationHandler(Box<dyn Fn() -> Option<gpui::accesskit::TreeUpdate> + Send>);

impl ActivationHandler for IosA11yActivationHandler {
    fn request_initial_tree(&mut self) -> Option<gpui::accesskit::TreeUpdate> {
        (self.0)()
    }
}

struct IosA11yActionHandler(Box<dyn Fn(gpui::accesskit::ActionRequest) + Send>);

impl ActionHandler for IosA11yActionHandler {
    fn do_action(&mut self, request: gpui::accesskit::ActionRequest) {
        (self.0)(request)
    }
}

struct IosA11yDeactivationHandler(Box<dyn Fn() + Send>);

impl DeactivationHandler for IosA11yDeactivationHandler {
    fn deactivate_accessibility(&mut self) {
        (self.0)()
    }
}

#[derive(Clone, Copy)]
struct KeyboardDismissTouch {
    id: TouchId,
    start_position: Point<Pixels>,
}

#[derive(Clone, Copy)]
struct KeyboardInsetAnimation {
    started_at: Instant,
    duration: Duration,
    from: f32,
    to: f32,
}

/// Global storage for the current status bar style.
/// 0 = default (dark content), 1 = light content.
/// Accessed from the main thread only.
static STATUS_BAR_STYLE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

#[derive(Default)]
pub(super) struct WindowReference(pub(super) RefCell<Weak<IosWindowState>>);

impl WindowReference {
    pub(super) fn with_window<R>(&self, callback: impl FnOnce(&IosWindowState) -> R) -> Option<R> {
        let window = self.0.borrow().upgrade()?;
        // Keep callback storage and native objects alive even if the callback
        // synchronously closes the platform window.
        Some(callback(&window))
    }
}

define_class!(
    #[unsafe(super = UIViewController)]
    #[thread_kind = MainThreadOnly]
    #[name = "GPUIViewController"]
    #[ivars = WindowReference]
    struct ViewController;

    impl ViewController {
        #[unsafe(method(preferredStatusBarStyle))]
        fn preferred_status_bar_style(&self) -> UIStatusBarStyle {
            let style = STATUS_BAR_STYLE.load(std::sync::atomic::Ordering::Relaxed);
            if style == 1 {
                UIStatusBarStyle::LightContent
            } else {
                UIStatusBarStyle::DarkContent
            }
        }

        #[unsafe(method(viewDidLayoutSubviews))]
        fn view_did_layout_subviews(&self) {
            unsafe {
                let _: () = msg_send![super(self), viewDidLayoutSubviews];
            }
            self.ivars().with_window(IosWindowState::handle_layout_change);
        }
    }
);

impl ViewController {
    fn new(main_thread: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(main_thread).set_ivars(WindowReference::default());
        unsafe { msg_send![super(this), init] }
    }
}

/// Set the iOS status bar content style (light or dark text/icons).
///
/// This updates the stored style and asks the root view controller
/// to re-query `preferredStatusBarStyle`.
pub fn set_status_bar_style(style: crate::StatusBarContentStyle) {
    use crate::StatusBarContentStyle;

    let value = match style {
        StatusBarContentStyle::Light => 1,
        StatusBarContentStyle::Dark => 0,
    };
    STATUS_BAR_STYLE.store(value, std::sync::atomic::Ordering::Relaxed);

    // This public free function predates explicit platform ownership.
    // Keep its single-application behavior without making runtime events global.
    if let Some(platform) = super::application::current_platform() {
        platform.application.with_windows(|window| {
            window.view_controller.setNeedsStatusBarAppearanceUpdate();
        });
    }
}

define_class!(
    #[unsafe(super = UIView)]
    #[thread_kind = MainThreadOnly]
    #[name = "GPUIMetalView"]
    #[ivars = WindowReference]
    struct MetalView;

    unsafe impl NSObjectProtocol for MetalView {}

    unsafe impl UITextInteractionDelegate for MetalView {
        #[unsafe(method(interactionShouldBegin:atPoint:))]
        fn interaction_should_begin_at_point(
            &self,
            _interaction: &UITextInteraction,
            location: CGPoint,
        ) -> bool {
            let point = gpui::point(px(location.x as f32), px(location.y as f32));
            let state = self.ivars().with_window(|window| {
                window.with_input_handler(|handler| {
                    (handler.query_accepts_focused_text_input(), handler.element_bounds())
                })
            }).flatten();
            let (focused, bounds) = state.unwrap_or((false, None));
            focused && bounds.is_some_and(|bounds| bounds.contains(&point))
        }

        #[unsafe(method(interactionWillBegin:))]
        fn interaction_will_begin(&self, _interaction: &UITextInteraction) {
            let should_show_keyboard = self
                .ivars()
                .with_window(|window| {
                    window
                        .with_input_handler(|handler| {
                            handler.query_accepts_focused_text_input()
                        })
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            if should_show_keyboard {
                self.ivars().with_window(IosWindowState::show_keyboard);
            }
        }
    }

    impl MetalView {
        #[unsafe(method(layerClass))]
        fn layer_class() -> &'static AnyClass {
            CAMetalLayer::class()
        }

        // This callback also covers iOS 15/16, before UIKit's trait-registration API.
        #[unsafe(method(traitCollectionDidChange:))]
        fn trait_collection_did_change(&self, previous: Option<&UITraitCollection>) {
            unsafe {
                let _: () = msg_send![super(self), traitCollectionDidChange: previous];
                if self
                    .traitCollection()
                    .hasDifferentColorAppearanceComparedToTraitCollection(previous)
                {
                    self.ivars()
                        .with_window(IosWindowState::notify_appearance_changed);
                }
            }
        }

        #[unsafe(method(touchesBegan:withEvent:))]
        fn touches_began(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            self.handle_touches(touches, event);
        }

        #[unsafe(method(touchesMoved:withEvent:))]
        fn touches_moved(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            self.handle_touches(touches, event);
        }

        #[unsafe(method(touchesEnded:withEvent:))]
        fn touches_ended(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            self.handle_touches(touches, event);
        }

        #[unsafe(method(touchesCancelled:withEvent:))]
        fn touches_cancelled(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            self.handle_touches(touches, event);
        }

    }
);

impl MetalView {
    fn new(frame: CGRect, main_thread: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(main_thread).set_ivars(WindowReference::default());
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn handle_touches(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
        for touch in touches {
            self.ivars()
                .with_window(|window| window.handle_touch(&touch, event));
        }
    }
}

pub(crate) struct IosWindow {
    state: Rc<IosWindowState>,
}

impl std::ops::Deref for IosWindow {
    type Target = IosWindowState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

#[allow(clippy::type_complexity)]
pub(crate) struct IosWindowState {
    platform: Weak<IosPlatformState>,
    handle: AnyWindowHandle,
    keyboard_observers:
        RefCell<Vec<Retained<objc2::runtime::ProtocolObject<dyn NSObjectProtocol>>>>,
    /// The UIWindow object
    window: Retained<UIWindow>,
    /// The UIViewController
    view_controller: Retained<ViewController>,
    /// The Metal-backed UIView
    view: Retained<MetalView>,
    /// The hidden text input view for keyboard input
    text_input_view: Retained<TextInputView>,
    /// UIKit selection gestures attached to the visible Metal view and backed by the text proxy.
    text_interaction: Retained<UITextInteraction>,
    /// Current bounds in pixels
    bounds: Cell<Bounds<Pixels>>,
    /// Scale factor
    scale_factor: Cell<f32>,
    /// Input handler for text input
    input_handler: CallbackSlot<PlatformInputHandler>,
    request_frame_callback: CallbackSlot<Box<dyn FnMut(RequestFrameOptions)>>,
    force_next_frame: Cell<bool>,
    /// Callback for input events
    input_callback: CallbackSlot<Box<dyn FnMut(PlatformInput) -> DispatchEventResult>>,
    a11y_adapter: RefCell<Option<SubclassingAdapter>>,
    /// Callback for active status changes
    active_status_callback: CallbackSlot<Box<dyn FnMut(bool)>>,
    visibility: Cell<WindowVisibility>,
    visibility_callback: CallbackSlot<Box<dyn FnMut(WindowVisibility)>>,
    /// Callback for hover status changes (not really applicable on iOS)
    hover_status_callback: CallbackSlot<Box<dyn FnMut(bool)>>,
    /// Callback for resize events
    resize_callback: CallbackSlot<Box<dyn FnMut(Size<Pixels>, f32)>>,
    display_id: Cell<DisplayId>,
    display_changed_callback: CallbackSlot<Box<dyn FnMut()>>,
    /// Callback for move events (not applicable on iOS)
    moved_callback: CallbackSlot<Box<dyn FnMut()>>,
    /// Callback for should close
    should_close_callback: CallbackSlot<Box<dyn FnMut() -> bool>>,
    /// Callback for hit test
    hit_test_callback: CallbackSlot<Box<dyn FnMut() -> Option<WindowControlArea>>>,
    /// Callback for close
    close_callback: CallbackSlot<Box<dyn FnOnce()>>,
    /// Callback for appearance changes
    appearance_changed_callback: CallbackSlot<Box<dyn FnMut()>>,
    insets_changed_callback: CallbackSlot<Box<dyn FnMut(WindowInsets)>>,
    keyboard_dismiss_touch: Cell<Option<KeyboardDismissTouch>>,
    keyboard_height: Cell<f32>,
    keyboard_animation: Cell<Option<KeyboardInsetAnimation>>,
    /// Current mouse position (from touch)
    mouse_position: Cell<Point<Pixels>>,
    /// Current modifiers
    modifiers: Cell<Modifiers>,
    renderer: Mutex<MetalRenderer>,
}

impl IosWindow {
    #[allow(deprecated)] // Window construction can precede scene connection.
    pub fn new(
        handle: AnyWindowHandle,
        _params: WindowParams,
        platform: &Rc<IosPlatformState>,
    ) -> anyhow::Result<Self> {
        // Create the window on the main screen
        let screen = IosDisplay::main();
        let screen_bounds = screen.bounds();
        let scale_factor = screen.scale();

        let main_thread = MainThreadMarker::new().expect("UIKit requires the main thread");
        // Create UIWindow
        let window_scene = platform.application.window_scene();
        let window_scene = window_scene.as_deref();
        let (window, screen_bounds_cg, scale) = if let Some(scene) = window_scene {
            let screen = scene.screen();
            let bounds = screen.bounds();
            let window = UIWindow::initWithWindowScene(UIWindow::alloc(main_thread), scene);
            window.setFrame(bounds);
            (window, bounds, screen.scale())
        } else {
            let screen = UIScreen::mainScreen(main_thread);
            let bounds = screen.bounds();
            let window = UIWindow::initWithFrame(UIWindow::alloc(main_thread), bounds);
            (window, bounds, screen.scale())
        };

        let metal_frame = CGRect::new(CGPoint::ZERO, screen_bounds_cg.size);
        let view = MetalView::new(metal_frame, main_thread);

        let layer = view.layer();
        layer.setContentsScale(scale);

        // Auto-resize the Metal view when the parent view changes size
        // (e.g. rotation). UIViewAutoresizingFlexibleWidth | UIViewAutoresizingFlexibleHeight
        view.setAutoresizingMask(
            UIViewAutoresizing::FlexibleWidth | UIViewAutoresizing::FlexibleHeight,
        );

        // Enable user interaction on the Metal view for touch handling
        view.setUserInteractionEnabled(true);
        view.setMultipleTouchEnabled(true);

        let view_controller = ViewController::new(main_thread);
        view_controller.setView(Some(&view));
        window.setRootViewController(Some(&view_controller));

        // Create a hidden text input view for keyboard handling.
        // Uses our custom GPUITextInputView which implements UIKeyInput
        // so iOS actually routes keyboard text to us.
        let text_input_frame = CGRect::new(CGPoint::ZERO, CGSize::new(1.0, 1.0));
        let text_input_view = TextInputView::new(text_input_frame, main_thread);
        text_input_view.setAlpha(0.01);
        text_input_view.setUserInteractionEnabled(true);
        view.addSubview(&text_input_view);

        let text_interaction =
            UITextInteraction::textInteractionForMode(UITextInteractionMode::Editable, main_thread);
        unsafe { text_interaction.setTextInput(Some(text_input_view.as_super().as_super())) };
        text_interaction.setDelegate(Some(ProtocolObject::from_ref(&*view)));
        view.addInteraction(&ProtocolObject::from_ref(&*text_interaction));

        let pixel_w = (screen_bounds_cg.size.width * scale) as i32;
        let pixel_h = (screen_bounds_cg.size.height * scale) as i32;
        let renderer_layer = layer
            .downcast::<CAMetalLayer>()
            .map_err(|_| anyhow::anyhow!("UIKit view must have a CAMetalLayer"))?;
        let mut renderer =
            MetalRenderer::new_for_layer(renderer_layer, Arc::new(Mutex::default()), false);
        renderer.update_drawable_size(size(DevicePixels(pixel_w), DevicePixels(pixel_h)));

        let state = IosWindowState {
            platform: Rc::downgrade(platform),
            handle,
            keyboard_observers: RefCell::default(),
            display_id: Cell::new(IosDisplay::from_screen(window.screen()).id()),
            display_changed_callback: CallbackSlot::default(),
            window,
            view_controller,
            view,
            text_input_view,
            text_interaction,
            bounds: Cell::new(screen_bounds),
            scale_factor: Cell::new(scale_factor),
            input_handler: CallbackSlot::default(),
            request_frame_callback: CallbackSlot::default(),
            force_next_frame: Cell::new(true),
            input_callback: CallbackSlot::default(),
            a11y_adapter: RefCell::default(),
            active_status_callback: CallbackSlot::default(),
            visibility: Cell::new(WindowVisibility::Visible),
            visibility_callback: CallbackSlot::default(),
            hover_status_callback: CallbackSlot::default(),
            resize_callback: CallbackSlot::default(),
            moved_callback: CallbackSlot::default(),
            should_close_callback: CallbackSlot::default(),
            hit_test_callback: CallbackSlot::default(),
            close_callback: CallbackSlot::default(),
            appearance_changed_callback: CallbackSlot::default(),
            insets_changed_callback: CallbackSlot::default(),
            keyboard_dismiss_touch: Cell::new(None),
            keyboard_height: Cell::new(0.),
            keyboard_animation: Cell::new(None),
            mouse_position: Cell::new(Point::default()),
            modifiers: Cell::new(Modifiers::default()),
            renderer: Mutex::new(renderer),
        };

        Ok(Self {
            state: Rc::new(state),
        })
    }

    pub(crate) fn register(&self) {
        if let Some(platform) = self.platform.upgrade() {
            platform.application.register_window(&self.state);
        }

        let window = Rc::downgrade(&self.state);
        *self.view_controller.ivars().0.borrow_mut() = window.clone();
        *self.view.ivars().0.borrow_mut() = window.clone();
        self.text_input_view.set_window(window);

        self.state.register_keyboard_observers();
    }
}

impl IosWindowState {
    pub(super) fn handle(&self) -> AnyWindowHandle {
        self.handle
    }

    pub(super) fn is_active(&self) -> bool {
        self.window.isKeyWindow()
    }

    pub(super) fn attach_to_scene(&self, scene: &UIWindowScene) {
        self.window.setWindowScene(Some(scene));
        self.window.makeKeyAndVisible();
        self.notify_visibility_change(WindowVisibility::Visible);
        self.handle_layout_change();
    }

    fn register_keyboard_observers(self: &Rc<Self>) {
        unsafe {
            let notification_center = NSNotificationCenter::defaultCenter();

            let window = Rc::downgrade(self);
            let frame_change_block =
                block2::RcBlock::new(move |notification: NonNull<NSNotification>| {
                    let notification = notification.as_ref();
                    let Some(user_info) = notification.userInfo() else {
                        return;
                    };
                    let Some(frame_value) = user_info.objectForKey(UIKeyboardFrameEndUserInfoKey)
                    else {
                        return;
                    };
                    let Some(frame_value) = frame_value.downcast_ref::<NSValue>() else {
                        return;
                    };
                    let raw_frame = frame_value.CGRectValue();
                    if let Some(window) = window.upgrade() {
                        let screen_space = window.window.screen().coordinateSpace();
                        let converted_frame = window
                            .window
                            .convertRect_fromCoordinateSpace(raw_frame, &screen_space);
                        let bounds = window.view.bounds();
                        let inset = keyboard_bottom_inset(bounds, converted_frame);
                        window.animate_keyboard_height(
                            inset,
                            keyboard_animation_duration(notification),
                        );
                    }
                });

            let window = Rc::downgrade(self);
            let hide_block = block2::RcBlock::new(move |notification: NonNull<NSNotification>| {
                if let Some(window) = window.upgrade() {
                    window.animate_keyboard_height(
                        0.,
                        keyboard_animation_duration(notification.as_ref()),
                    );
                }
            });

            let frame_observer = notification_center.addObserverForName_object_queue_usingBlock(
                Some(UIKeyboardWillChangeFrameNotification),
                None,
                None,
                &frame_change_block,
            );
            let hide_observer = notification_center.addObserverForName_object_queue_usingBlock(
                Some(UIKeyboardWillHideNotification),
                None,
                None,
                &hide_block,
            );
            self.keyboard_observers
                .borrow_mut()
                .extend([frame_observer, hide_observer]);
        }
    }

    /// Delivers a UIKit touch through GPUI's platform-neutral touch API.
    pub fn handle_touch(&self, touch: &UITouch, _event: Option<&UIEvent>) {
        // GPUI timestamps delivery itself. Replaying UIKit's historical coalesced
        // samples here would make them appear simultaneous and distort fling velocity.
        let id = touch_id(touch);
        self.handle_touch_sample(touch, id);
    }

    fn handle_touch_sample(&self, touch: &UITouch, id: TouchId) {
        let position = touch_location_in_view(touch, &self.view);
        self.mouse_position.set(position);

        let event = TouchEvent {
            id,
            phase: touch_phase(touch),
            position,
            predicted_position: None,
            force: touch_force(touch),
        };
        self.handle_keyboard_dismiss_touch(&event);
        self.input_callback
            .with(|callback| callback(PlatformInput::Touch(event)));
    }

    fn handle_keyboard_dismiss_touch(&self, event: &TouchEvent) {
        if self.keyboard_height.get() <= 0. {
            self.keyboard_dismiss_touch.set(None);
            return;
        }

        match event.phase {
            TouchPhase::Started if self.keyboard_dismiss_touch.get().is_none() => {
                self.keyboard_dismiss_touch.set(Some(KeyboardDismissTouch {
                    id: event.id,
                    start_position: event.position,
                }));
            }
            TouchPhase::Moved => {
                let Some(touch) = self
                    .keyboard_dismiss_touch
                    .get()
                    .filter(|touch| touch.id == event.id)
                else {
                    return;
                };
                let delta = event.position - touch.start_position;
                if delta.y >= KEYBOARD_DISMISS_DISTANCE && delta.y.abs() > delta.x.abs() * 1.25 {
                    self.keyboard_dismiss_touch.set(None);
                    self.hide_keyboard();
                }
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if self
                    .keyboard_dismiss_touch
                    .get()
                    .is_some_and(|touch| touch.id == event.id)
                {
                    self.keyboard_dismiss_touch.set(None);
                }
            }
            TouchPhase::Started => {}
        }
    }

    pub(super) fn request_frame(&self) {
        self.advance_keyboard_animation();
        self.request_frame_callback.with(|callback| {
            let force_render = self.force_next_frame.replace(false);
            callback(RequestFrameOptions {
                force_render,
                ..Default::default()
            });
        });
    }

    /// Query the safe area insets from the UIView.
    ///
    /// Returns `(top, bottom, left, right)` in logical points.
    /// These represent the areas occupied by system UI (status bar,
    /// home indicator, camera notch) that content should avoid.
    fn safe_area_insets(&self) -> (f32, f32, f32, f32) {
        let insets = self.view.safeAreaInsets();
        (
            insets.top as f32,
            insets.bottom as f32,
            insets.left as f32,
            insets.right as f32,
        )
    }

    fn current_insets(&self) -> WindowInsets {
        let (top, bottom, left, right) = self.safe_area_insets();
        WindowInsets {
            safe_area: Edges {
                top: px(top),
                right: px(right),
                bottom: px(bottom),
                left: px(left),
            },
            ime: Edges {
                bottom: px(self.keyboard_height.get()),
                ..Default::default()
            },
        }
    }

    fn notify_insets_changed(&self) {
        self.insets_changed_callback
            .with(|callback| callback(self.current_insets()));
    }

    fn notify_appearance_changed(&self) {
        self.appearance_changed_callback.with(|callback| callback());
    }

    fn set_keyboard_height(&self, height: f32) {
        self.keyboard_animation.set(None);
        self.apply_keyboard_height(height);
    }

    fn animate_keyboard_height(&self, height: f32, duration: Duration) {
        let height = height.max(0.);
        let from = self.keyboard_height.get();
        if duration.is_zero() || (from - height).abs() <= 0.1 {
            self.set_keyboard_height(height);
            return;
        }
        self.keyboard_animation.set(Some(KeyboardInsetAnimation {
            started_at: Instant::now(),
            duration,
            from,
            to: height,
        }));
        self.force_next_frame.set(true);
    }

    fn advance_keyboard_animation(&self) {
        let Some(animation) = self.keyboard_animation.get() else {
            return;
        };
        let progress =
            animation.started_at.elapsed().as_secs_f64() / animation.duration.as_secs_f64();
        if progress >= 1.0 {
            self.keyboard_animation.set(None);
            self.apply_keyboard_height(animation.to);
        } else {
            let t = progress as f32;
            let eased = t * t * (3. - 2. * t);
            self.apply_keyboard_height(animation.from + (animation.to - animation.from) * eased);
        }
    }

    fn apply_keyboard_height(&self, height: f32) {
        let height = height.max(0.);
        if (self.keyboard_height.get() - height).abs() <= 0.1 {
            return;
        }
        self.keyboard_height.set(height);
        if height <= 0.1 {
            self.keyboard_dismiss_touch.set(None);
        }
        self.notify_insets_changed();
    }

    pub fn show_keyboard(&self) {
        self.text_input_view.set_keyboard_visible(true);
    }

    pub fn hide_keyboard(&self) {
        self.text_input_view.set_keyboard_visible(false);
    }

    pub(super) fn with_input_handler<R>(
        &self,
        callback: impl FnOnce(&mut PlatformInputHandler) -> R,
    ) -> Option<R> {
        self.input_handler.with(callback)
    }

    /// Notify the window when its UIKit scene becomes active or inactive.
    pub fn notify_active_status_change(&self, is_active: bool) {
        log::info!("GPUI iOS: Window active status changed to: {}", is_active);

        self.active_status_callback
            .with(|callback| callback(is_active));
    }

    pub(crate) fn notify_visibility_change(&self, visibility: WindowVisibility) {
        if self.visibility.replace(visibility) != visibility {
            self.visibility_callback
                .with(|callback| callback(visibility));
        }
    }

    pub fn handle_layout_change(&self) {
        let view_bounds = self.view.bounds();
        let display = IosDisplay::from_screen(self.window.screen());
        if self.display_id.replace(display.id()) != display.id() {
            self.display_changed_callback.with(|callback| callback());
        }
        let scale = display.scale() as f64;

        let width = view_bounds.size.width as f32;
        let height = view_bounds.size.height as f32;
        let new_scale = scale as f32;

        let old_bounds = self.bounds.get();
        let old_scale = self.scale_factor.get();

        let new_size = size(px(width), px(height));
        self.notify_insets_changed();

        if old_bounds.size == new_size && (old_scale - new_scale).abs() < 0.01 {
            return;
        }

        log::info!(
            "GPUI iOS: Layout changed — {:?} @{:.1}x → {:?} @{:.1}x",
            old_bounds.size,
            old_scale,
            new_size,
            new_scale,
        );

        // Update stored bounds (in logical pixels, matching GPUI convention).
        let new_bounds = Bounds {
            origin: Default::default(),
            size: new_size,
        };
        self.bounds.set(new_bounds);
        self.scale_factor.set(new_scale);
        // Update the Metal layer's contentsScale so the drawable has the
        // correct pixel dimensions.
        self.view.layer().setContentsScale(scale);

        let pixel_width = (width * new_scale) as i32;
        let pixel_height = (height * new_scale) as i32;
        self.renderer
            .lock()
            .update_drawable_size(size(DevicePixels(pixel_width), DevicePixels(pixel_height)));

        self.resize_callback
            .with(|callback| callback(new_size, new_scale));
    }
}

impl Drop for IosWindow {
    fn drop(&mut self) {
        if let Some(platform) = self.platform.upgrade() {
            platform.application.unregister_window(&self.state);
        }
        let observers = std::mem::take(&mut *self.keyboard_observers.borrow_mut());
        for observer in observers {
            unsafe { NSNotificationCenter::defaultCenter().removeObserver((&*observer).as_ref()) };
        }

        *self.view_controller.ivars().0.borrow_mut() = Weak::new();
        *self.view.ivars().0.borrow_mut() = Weak::new();
        self.view
            .removeInteraction(&ProtocolObject::from_ref(&*self.text_interaction));
        self.text_interaction.setDelegate(None);
        unsafe { self.text_interaction.setTextInput(None) };
        self.text_input_view.set_window(Weak::new());
        self.text_input_view.removeFromSuperview();
    }
}

impl HasWindowHandle for IosWindow {
    fn window_handle(
        &self,
    ) -> std::result::Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError>
    {
        let view = NonNull::new(Retained::as_ptr(&self.view).cast_mut().cast::<c_void>())
            .ok_or(raw_window_handle::HandleError::Unavailable)?;
        let handle = UiKitWindowHandle::new(view);
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(handle.into()) })
    }
}

impl HasDisplayHandle for IosWindow {
    fn display_handle(
        &self,
    ) -> std::result::Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError>
    {
        let handle = UiKitDisplayHandle::new();
        Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(handle.into()) })
    }
}

impl PlatformWindow for IosWindow {
    fn bounds(&self) -> Bounds<Pixels> {
        self.bounds.get()
    }

    fn is_maximized(&self) -> bool {
        true // iOS windows are always "maximized"
    }

    fn window_bounds(&self) -> WindowBounds {
        WindowBounds::Fullscreen(self.bounds.get())
    }

    fn content_size(&self) -> Size<Pixels> {
        self.bounds.get().size
    }

    fn resize(&mut self, _size: Size<Pixels>) {
        // iOS windows cannot be resized programmatically
    }

    fn scale_factor(&self) -> f32 {
        self.scale_factor.get()
    }

    fn appearance(&self) -> WindowAppearance {
        match unsafe { self.view.traitCollection().userInterfaceStyle() } {
            UIUserInterfaceStyle::Dark => WindowAppearance::Dark,
            _ => WindowAppearance::Light,
        }
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(Rc::new(IosDisplay::from_screen(self.window.screen())))
    }

    fn on_display_changed(&self, callback: Box<dyn FnMut()>) {
        self.display_changed_callback.set(callback);
    }

    fn mouse_position(&self) -> Point<Pixels> {
        self.mouse_position.get()
    }

    fn modifiers(&self) -> Modifiers {
        self.modifiers.get()
    }

    fn capslock(&self) -> Capslock {
        // Would need to check UIKeyModifierFlags
        Capslock { on: false }
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        self.text_input_view
            .set_input_field(input_handler.focus_id());
        self.input_handler.set(input_handler);
        self.text_input_view.refresh_keyboard();
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        let handler = self.input_handler.take();
        self.text_input_view.refresh_keyboard();
        handler
    }

    fn prompt(
        &self,
        _level: PromptLevel,
        _msg: &str,
        _detail: Option<&str>,
        _answers: &[PromptButton],
    ) -> Option<futures::channel::oneshot::Receiver<usize>> {
        None
    }

    fn activate(&self, _token: Option<&str>) -> bool {
        self.window.makeKeyAndVisible();
        true
    }

    fn is_active(&self) -> bool {
        self.window.isKeyWindow()
    }

    fn is_hovered(&self) -> bool {
        // Hover isn't really applicable on iOS
        false
    }

    fn set_title(&mut self, _title: &str) {
        // iOS apps don't have window titles
    }

    fn background_appearance(&self) -> WindowBackgroundAppearance {
        WindowBackgroundAppearance::Opaque
    }

    fn set_background_appearance(&self, _background_appearance: WindowBackgroundAppearance) {
        // Could adjust view background color
    }

    fn minimize(&self) {
        // iOS apps cannot be minimized
    }

    fn zoom(&self) {
        // iOS apps cannot be zoomed
    }

    fn toggle_fullscreen(&self) {
        // iOS apps are always fullscreen
    }

    fn is_fullscreen(&self) -> bool {
        true
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.request_frame_callback.set(callback);
    }

    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> DispatchEventResult>) {
        self.input_callback.set(callback);
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.active_status_callback.set(callback);
        self.window.makeKeyAndVisible();
        self.notify_active_status_change(self.window.isKeyWindow());
    }

    fn visibility(&self) -> WindowVisibility {
        self.visibility.get()
    }

    fn on_visibility_change(&self, callback: Box<dyn FnMut(WindowVisibility)>) {
        self.visibility_callback.set(callback);
    }

    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.hover_status_callback.set(callback);
    }

    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.resize_callback.set(callback);
    }

    fn on_moved(&self, callback: Box<dyn FnMut()>) {
        self.moved_callback.set(callback);
    }

    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.should_close_callback.set(callback);
    }

    fn on_hit_test_window_control(&self, callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
        self.hit_test_callback.set(callback);
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.close_callback.set(callback);
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.appearance_changed_callback.set(callback);
    }

    fn draw(&self, scene: &Scene) {
        self.renderer.lock().draw(scene);
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.renderer.lock().sprite_atlas().clone()
    }

    fn is_subpixel_rendering_supported(&self) -> bool {
        false
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        None
    }

    fn update_ime_position(&self, _bounds: Bounds<Pixels>) {
        // iOS handles IME positioning automatically
    }

    fn insets(&self) -> WindowInsets {
        self.current_insets()
    }

    fn on_insets_changed(&self, callback: Box<dyn FnMut(WindowInsets)>) {
        self.insets_changed_callback.set(callback);
    }

    fn show_soft_keyboard(&self) {
        self.show_keyboard();
    }

    fn hide_soft_keyboard(&self) {
        self.hide_keyboard();
    }

    fn text_input_state_changed(&self, change: TextInputStateChange) {
        self.text_input_view.state_changed(change);
    }

    fn set_text_input_configuration(&mut self, configuration: TextInputConfiguration) {
        self.text_input_view.set_configuration(&configuration);
    }

    fn a11y_init(&self, callbacks: A11yCallbacks) {
        let adapter = unsafe {
            SubclassingAdapter::for_window(
                Retained::<UIWindow>::as_ptr(&self.window)
                    .cast_mut()
                    .cast::<c_void>(),
                IosA11yActivationHandler(callbacks.activation),
                IosA11yActionHandler(callbacks.action),
                IosA11yDeactivationHandler(callbacks.deactivation),
            )
        };
        self.a11y_adapter.borrow_mut().replace(adapter);
    }

    fn a11y_tree_update(&self, tree_update: gpui::accesskit::TreeUpdate) {
        let events = self
            .a11y_adapter
            .borrow()
            .as_ref()
            .and_then(|adapter| adapter.update_if_active(|| tree_update));
        if let Some(events) = events {
            events.raise();
        }
    }
}

fn keyboard_animation_duration(notification: &NSNotification) -> Duration {
    notification
        .userInfo()
        .and_then(|user_info| unsafe {
            user_info.objectForKey(UIKeyboardAnimationDurationUserInfoKey)
        })
        .and_then(|value| {
            value
                .downcast_ref::<NSNumber>()
                .map(|number| number.doubleValue())
        })
        .filter(|seconds| seconds.is_finite() && *seconds > 0.)
        .map_or(Duration::ZERO, |seconds| {
            Duration::from_secs_f64(seconds.min(1.))
        })
}

/// How much of the bottom of `bounds` a docked keyboard covers. A floating or split keyboard
/// doesn't span the view's width down to its bottom edge, so it covers nothing.
fn keyboard_bottom_inset(bounds: CGRect, keyboard: CGRect) -> f32 {
    let bottom = bounds.origin.y + bounds.size.height;
    let docked = keyboard.origin.x <= bounds.origin.x + 0.5
        && keyboard.origin.x + keyboard.size.width >= bounds.origin.x + bounds.size.width - 0.5
        && keyboard.origin.y + keyboard.size.height >= bottom - 0.5;
    let inset = bottom - keyboard.origin.y;
    if docked && inset > 0. {
        inset.min(bounds.size.height) as f32
    } else {
        0.
    }
}

#[cfg(test)]
mod tests {
    use super::{CallbackSlot, keyboard_bottom_inset};
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};

    #[test]
    fn keyboard_inset_uses_only_full_width_bottom_intersection() {
        let bounds = CGRect::new(CGPoint::new(20.0, 40.0), CGSize::new(400.0, 800.0));
        let docked = CGRect::new(CGPoint::new(20.0, 540.0), CGSize::new(400.0, 400.0));
        assert_eq!(keyboard_bottom_inset(bounds, docked), 300.0);

        let floating = CGRect::new(CGPoint::new(80.0, 340.0), CGSize::new(240.0, 220.0));
        assert_eq!(keyboard_bottom_inset(bounds, floating), 0.0);

        let split = CGRect::new(CGPoint::new(20.0, 540.0), CGSize::new(170.0, 400.0));
        assert_eq!(keyboard_bottom_inset(bounds, split), 0.0);
    }

    #[test]
    fn keyboard_inset_rejects_invalid_geometry() {
        let bounds = CGRect::new(CGPoint::ZERO, CGSize::new(400.0, 800.0));
        let invalid = CGRect::new(CGPoint::new(0.0, f64::NAN), CGSize::new(400.0, 300.0));
        assert_eq!(keyboard_bottom_inset(bounds, invalid), 0.0);
    }

    #[test]
    fn callback_is_unavailable_during_dispatch_and_restored_afterward() {
        let slot = CallbackSlot::default();
        slot.set(1);
        assert_eq!(
            slot.with(|value| {
                *value += 1;
                assert!(
                    slot.with(|_| panic!("reentered the same handler"))
                        .is_none()
                );
                *value
            }),
            Some(2)
        );
        assert_eq!(slot.with(|value| *value), Some(2));
    }

    #[test]
    fn callback_replacement_during_dispatch_is_preserved() {
        let slot = CallbackSlot::default();
        slot.set(1);
        slot.with(|_| {
            slot.set(2);
            slot.with(|value| *value += 1);
        });
        assert_eq!(slot.take(), Some(3));
    }

    #[test]
    fn withdrawing_an_in_flight_handler_does_not_restore_it() {
        let slot = CallbackSlot::default();
        slot.set(1);
        slot.with(|_| assert!(slot.take().is_none()));
        assert!(
            slot.with(|_| panic!("restored a withdrawn handler"))
                .is_none()
        );
    }

    #[test]
    fn removing_a_replacement_does_not_resurrect_the_previous_handler() {
        let slot = CallbackSlot::default();
        slot.set(1);
        slot.with(|_| {
            slot.set(2);
            assert_eq!(slot.take(), Some(2));
        });
        assert!(slot.take().is_none());
    }
}
