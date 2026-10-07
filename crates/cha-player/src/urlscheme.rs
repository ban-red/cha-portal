//! The `cha://` URL scheme: macOS hands a clicked link to the app as a
//! `GetURL` Apple Event, not as a command-line argument.
//!
//! The handler is installed on the main thread before winit's event loop
//! runs, so a link that launches the app cold is delivered too (the event
//! waits for the loop to start). Each URL goes to the app as a user event.

use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::{MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{MainThreadMarker, NSAppleEventDescriptor, NSAppleEventManager, NSObject};

/// `kInternetEventClass` and `kAEGetURL`, both 'GURL'.
const GURL: u32 = u32::from_be_bytes(*b"GURL");
/// `keyDirectObject`, '----': the URL.
const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

type Sink = Box<dyn Fn(String) + Send + Sync>;
static SINK: OnceLock<Sink> = OnceLock::new();

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ChaPlayerUrlHandler"]
    struct UrlHandler;

    impl UrlHandler {
        #[unsafe(method(handleGetURLEvent:withReplyEvent:))]
        fn handle_get_url(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            let url = event
                .paramDescriptorForKeyword(DIRECT_OBJECT)
                .and_then(|d| d.stringValue())
                .map(|s| s.to_string());
            if let (Some(url), Some(sink)) = (url, SINK.get()) {
                sink(url);
            }
        }
    }
);

/// Starts delivering URLs to `deliver` (which runs on the main thread).
/// Call once, on the main thread, before the event loop runs.
pub fn install(deliver: impl Fn(String) + Send + Sync + 'static) {
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("cha:// links need the URL handler installed on the main thread");
        return;
    };
    if SINK.set(Box::new(deliver)).is_err() {
        return;
    }
    let handler: Retained<UrlHandler> = unsafe { msg_send![mtm.alloc::<UrlHandler>(), init] };
    // SAFETY: the selector is the one `UrlHandler` defines, with the
    // (event, reply) signature the manager calls it with.
    unsafe {
        NSAppleEventManager::sharedAppleEventManager()
            .setEventHandler_andSelector_forEventClass_andEventID(
                &handler,
                sel!(handleGetURLEvent:withReplyEvent:),
                GURL,
                GURL,
            );
    }
    // The manager doesn't retain its handler, and this one lives as long as the app.
    std::mem::forget(handler);
}
