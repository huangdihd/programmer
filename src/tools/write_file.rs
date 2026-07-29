// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use async_openai::types::responses::Tool;
use serde::Deserialize;
use serde_json::json;

use super::function_tool;

pub const NAME: &str = "write_file";

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Write text to a file, creating it (and any missing parent directories) \
         or replacing its ENTIRE contents if it already exists. Unlike edit_file, \
         this replaces the whole file — use edit_file for targeted changes.",
        json!({
            "path": {
                "type": "string",
                "description": "Path to the file to write."
            },
            "content": {
                "type": "string",
                "description": "The full contents to write to the file."
            }
        }),
        &["path", "content"],
    )
}

#[derive(Deserialize)]
struct Args {
    path: String,
    content: String,
}

pub async fn run(arguments: &str) -> Result<String, String> {
    let security = crate::security::SecurityManager::for_current_dir(Default::default())?;
    run_with_security(arguments, &security).await
}

pub(crate) async fn run_with_security(
    arguments: &str,
    security: &crate::security::SecurityManager,
) -> Result<String, String> {
    let args: Args = match serde_json::from_str(arguments) {
        Ok(args) => args,
        Err(error) => return Err(format!("error: invalid arguments: {error}")),
    };

    let path = security.authorize_path(crate::security::policy::AccessKind::Write, &args.path)?;
    let current = match tokio::fs::read(&path).await {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "error: could not inspect {}: {error}",
                path.display()
            ));
        }
    };
    security.validate_write(&path, current.as_deref())?;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && let Err(error) = tokio::fs::create_dir_all(parent).await
    {
        return Err(format!(
            "error: could not create {}: {error}",
            parent.display()
        ));
    }

    match tokio::fs::write(&path, &args.content).await {
        Ok(()) => {
            security.record_read(&path, args.content.as_bytes());
            Ok(format!(
                "wrote {} bytes to {}",
                args.content.len(),
                path.display()
            ))
        }
        Err(error) => Err(format!(
            "error: could not write {}: {error}",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refuses_unread_and_stale_existing_files() {
        let root =
            std::env::temp_dir().join(format!("programmer-write-guard-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let path = root.join("file.txt");
        tokio::fs::write(&path, "initial").await.unwrap();
        let security =
            crate::security::SecurityManager::new(Default::default(), root.clone()).unwrap();
        let write = serde_json::json!({"path": path, "content": "agent"}).to_string();

        let unread = run_with_security(&write, &security).await.unwrap_err();
        assert!(unread.contains("has not been read"));

        let read = serde_json::json!({"path": path}).to_string();
        crate::tools::read_file::run_with_security(&read, &security)
            .await
            .unwrap();
        tokio::fs::write(&path, "external").await.unwrap();

        let stale = run_with_security(&write, &security).await.unwrap_err();
        assert!(stale.contains("changed after the last read"));
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "external");
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
