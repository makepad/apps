use makepad_test::TestApp;
use super::HANG;

/// Waits until `look` answers `Ok`, and that answer; while it answers `Err(what)`, what is still
/// missing, the wait goes on. The app's main thread may spend the test's `HANG` of CPU on it, and
/// the runtime ends it sooner once the app sits idle.
pub fn wait_poll<T>(app: &TestApp, mut look: impl FnMut() -> Result<T, String>) -> T {
    let mut got = None;
    app.wait_until_within(HANG, || match look() {
        Ok(value) => {
            got = Some(value);
            None
        }
        Err(why) => Some(why),
    });
    got.expect("the wait ended on an answer")
}
