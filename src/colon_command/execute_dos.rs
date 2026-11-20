//! Executor for `Action::DosRun` (PR D2a): opens a DOS pane through
//! [`WindowManager::spawn_dos_pane`] and reports the outcome in the status bar.

use crate::wm::WindowManager;

#[cfg(feature = "dos")]
pub(super) fn execute_dos_run(
    wm: &mut WindowManager,
    name: Option<String>,
    image: Option<String>,
    drive_c: Option<String>,
    backend: Option<String>,
) {
    use crate::dos::launch::{RunRequest, with_host_env};
    // Host paths from the automation plane are refused until the D3 surface defines a policy
    // (an RPC client must not be able to point the emulator at arbitrary host files).
    if wm.is_api_origin() && (image.is_some() || drive_c.is_some()) {
        wm.active_pane_mut()
            .set_status_message(Some("dos run: --image/--drive-c are not allowed from the API".into()));
        return;
    }
    let req = RunRequest {
        name,
        image: image.map(Into::into),
        drive_c: drive_c.map(Into::into),
        backend,
    };
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            wm.active_pane_mut().set_status_message(Some(format!("dos run: cannot locate bilterm: {e}")));
            return;
        }
    };
    // The pane size is checked by `spawn_dos_pane` after the split; no terminal size here.
    let res = with_host_env(None, |env| wm.spawn_dos_pane(&req, env, &exe));
    let msg = match res {
        Ok(Ok(id)) => format!("DOS pane #{id} opened"),
        Ok(Err(e)) => format!("dos run: {e}"),
        Err(e) => format!("dos run: {e}"),
    };
    wm.active_pane_mut().set_status_message(Some(msg));
}

#[cfg(not(feature = "dos"))]
pub(super) fn execute_dos_run(
    wm: &mut WindowManager,
    _name: Option<String>,
    _image: Option<String>,
    _drive_c: Option<String>,
    _backend: Option<String>,
) {
    wm.active_pane_mut().set_status_message(Some(
        "built without the `dos` feature (rebuild with `cargo build --features dos`)".into(),
    ));
}
