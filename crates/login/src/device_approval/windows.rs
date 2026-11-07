//! Desktop UserConsentVerifier requires its interop interface and an owner HWND.
use super::{finish, Outcome};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::windows::WindowExtWindows;
use tao::window::WindowBuilder;
use windows::core::{factory, HSTRING};
use windows::Security::Credentials::UI::{
    UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
};
use windows::Win32::Foundation::{E_NOINTERFACE, HWND};
use windows::Win32::System::WinRT::{
    IUserConsentVerifierInterop, RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED,
};
use windows_future::IAsyncOperation;

struct Apartment;
impl Apartment {
    fn new() -> windows::core::Result<Self> {
        // SAFETY: called on fresh helper workers, with balanced initialization per thread.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}

pub(super) fn run(reason: &str) {
    // Availability is noninteractive. Run COM on a worker so Tao remains responsible for the
    // UI thread's apartment. A failed check is not permission to skip authentication.
    let availability = std::thread::spawn(|| -> windows::core::Result<_> {
        let _apartment = Apartment::new()?;
        let availability = UserConsentVerifier::CheckAvailabilityAsync()?.join()?;
        if availability == UserConsentVerifierAvailability::Available {
            // Querying the desktop interface is noninteractive. Do not guess OS support from
            // a version string or use the UWP request API in this desktop helper.
            let _: IUserConsentVerifierInterop = factory::<UserConsentVerifier, _>()?;
        }
        Ok(availability)
    })
    .join();
    match availability {
        Ok(Ok(UserConsentVerifierAvailability::DeviceNotPresent)) => {
            finish(Ok(Outcome::NotPresent))
        }
        Ok(Ok(UserConsentVerifierAvailability::NotConfiguredForUser)) => {
            finish(Ok(Outcome::NotConfigured))
        }
        Ok(Ok(UserConsentVerifierAvailability::Available)) => {}
        Ok(Err(error)) if error.code() == E_NOINTERFACE => {
            std::process::exit(protocol::device_approval::API_UNAVAILABLE_EXIT);
        }
        _ => finish(Err(
            "The Windows verifier is unavailable or busy.".to_owned()
        )),
    }

    let event_loop = EventLoopBuilder::<Result<Outcome, String>>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title("Approve adding a device to Tiny")
        .with_inner_size(tao::dpi::LogicalSize::new(360., 120.))
        .with_resizable(false)
        .build(&event_loop)
        .unwrap_or_else(|_| finish(Err("No approval window.".to_owned())));
    let hwnd = window.hwnd();
    let reason = reason.to_owned();
    let proxy = event_loop.create_proxy();
    std::thread::spawn(move || {
        let result = (|| -> windows::core::Result<Outcome> {
            let _apartment = Apartment::new()?;
            let interop: IUserConsentVerifierInterop = factory::<UserConsentVerifier, _>()?;
            let operation: IAsyncOperation<UserConsentVerificationResult> = unsafe {
                interop.RequestVerificationForWindowAsync(
                    HWND(hwnd as *mut _),
                    &HSTRING::from(reason),
                )?
            };
            match operation.join()? {
                UserConsentVerificationResult::Verified => Ok(Outcome::Verified),
                // Once a prompt is requested every other result aborts, including a race in
                // availability, busy hardware, cancellation and exhausted attempts.
                _ => Err(windows::core::Error::from_hresult(windows::core::HRESULT(
                    0x80004005u32 as i32,
                ))),
            }
        })()
        .map_err(|_| "Windows did not approve adding this device.".to_owned());
        let _ = proxy.send_event(result);
    });
    event_loop.run(move |event, _, control| {
        let _keep_window = &window;
        *control = ControlFlow::Wait;
        match event {
            Event::UserEvent(result) => finish(result),
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => finish(Err("Cancelled".to_owned())),
            _ => {}
        }
    });
}
