//! The desktop app: the web UI (`web/`) in a Tauri window. The Rust side holds memo-core with its
//! SQLite store (`memo-native`), does the HTTP and keeps the keys in the Secret Service; the
//! frontend's `NativeBackend` (`web/src/lib/native.ts`) drives it, offline-first, over one IPC
//! command. See `desktop/README.md`.

mod calls;
#[cfg(debug_assertions)]
mod serve;

use std::sync::Arc;

use tauri::{
    Manager,
    ipc::{InvokeBody, Request, Response},
};

use calls::{Ctx, Secrets};

/// Everything goes through here: the call's name is in the `memo-call` header, its bytes in the body.
#[tauri::command]
async fn call(request: Request<'_>, ctx: tauri::State<'_, Arc<Ctx>>) -> Result<Response, String> {
    let name =
        request.headers().get("memo-call").and_then(|v| v.to_str().ok()).ok_or("no memo-call header")?.to_owned();
    let body = match request.body() {
        InvokeBody::Raw(bytes) => bytes.clone(),
        InvokeBody::Json(_) => return Err("expected bytes".into()),
    };
    let ctx = Arc::clone(&ctx);
    // Argon2 and HTTP block: keep them off the main thread.
    tauri::async_runtime::spawn_blocking(move || calls::dispatch(&ctx, &name, &body))
        .await
        .map_err(|e| e.to_string())?
        .map(Response::new)
}

fn main() {
    #[cfg(debug_assertions)]
    if std::env::args().nth(1).as_deref() == Some("--serve") {
        return serve::main(std::env::args().skip(2).collect());
    }
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let downloads = app.path().download_dir().unwrap_or_else(|_| data_dir.clone());
            let ctx = Ctx::new(data_dir, downloads, Secrets::Keyring)?;
            app.manage(ctx);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![call])
        .run(tauri::generate_context!())
        .expect("the app could not start");
}
