//! An opaque view over the window while the scene is not active, so the app switcher
//! does not show a conversation. Drawn only when the screen lock is armed.

use objc2::rc::Retained;
use objc2::MainThreadOnly;
use objc2_ui_kit::{UIColor, UIScreen, UIView};
use std::cell::RefCell;

thread_local! {
    static COVER: RefCell<Option<Retained<UIView>>> = const { RefCell::new(None) };
}

pub(super) fn set(on: bool) {
    super::on_main(move |mtm| {
        COVER.with(|slot| {
            let mut slot = slot.borrow_mut();
            if !on {
                if let Some(view) = slot.take() {
                    view.removeFromSuperview();
                }
                return;
            }
            if slot.is_some() {
                return;
            }
            let Some(window) = super::auth::key_window(mtm) else {
                return;
            };
            let bounds = UIScreen::mainScreen(mtm).bounds();
            let view = UIView::initWithFrame(UIView::alloc(mtm), bounds);
            view.setBackgroundColor(Some(&UIColor::blackColor()));
            view.setUserInteractionEnabled(false);
            window.addSubview(&view);
            *slot = Some(view);
        });
    });
}
