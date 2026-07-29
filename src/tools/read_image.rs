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

use async_openai::types::responses::{FunctionCallOutput, InputContent, InputTextContent, Tool};
use image::GenericImageView;
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};

use super::function_tool;

pub const NAME: &str = "read_image";

pub fn tool() -> Tool {
    function_tool(
        NAME,
        "Read a local image and return it as visual input. Use this instead of \
         read_file when you need to inspect what an image contains. The path may \
         be absolute, relative to the working directory, or start with ~/.",
        json!({
            "path": {
                "type": "string",
                "description": "Path to the image to inspect."
            }
        }),
        &["path"],
    )
}

#[derive(Deserialize)]
struct Args {
    path: String,
}

pub async fn run(arguments: &str) -> Result<FunctionCallOutput, String> {
    let security = crate::security::SecurityManager::for_current_dir(Default::default())?;
    run_with_security(arguments, &security).await
}

pub(crate) async fn run_with_security(
    arguments: &str,
    security: &crate::security::SecurityManager,
) -> Result<FunctionCallOutput, String> {
    let args: Args = serde_json::from_str(arguments)
        .map_err(|error| format!("error: invalid arguments: {error}"))?;
    let path = security.authorize_path(
        crate::security::policy::AccessKind::Read,
        expand_tilde(&args.path),
    )?;
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|error| format!("error: could not read {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("error: {} is not a file", path.display()));
    }
    if metadata.len() > crate::commands::MAX_IMAGE_BYTES {
        return Err(format!(
            "error: image is {} bytes; limit is {}",
            metadata.len(),
            crate::commands::MAX_IMAGE_BYTES
        ));
    }

    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|error| format!("error: could not read {}: {error}", path.display()))?;
    security.record_read(&path, &bytes);
    let image = crate::commands::image_content_from_bytes(&bytes)
        .map_err(|error| format!("error: {error}"))?;
    let decoded = image::load_from_memory(&bytes)
        .map_err(|error| format!("error: could not decode {}: {error}", path.display()))?;
    let (width, height) = decoded.dimensions();
    let description = format!(
        "Read image {} ({width}x{height}, {} bytes).",
        path.display(),
        bytes.len()
    );

    Ok(FunctionCallOutput::Content(vec![
        InputContent::InputText(InputTextContent { text: description }),
        InputContent::InputImage(image),
    ]))
}

fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    Path::new(path).to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_openai::types::responses::InputContent;
    use image::{ImageBuffer, ImageFormat, Rgb};
    use std::io::Cursor;

    fn png() -> Vec<u8> {
        let image = ImageBuffer::from_pixel(2, 1, Rgb([10u8, 20, 30]));
        let mut bytes = Cursor::new(Vec::new());
        image
            .write_to(&mut bytes, ImageFormat::Png)
            .expect("encode PNG");
        bytes.into_inner()
    }

    #[tokio::test]
    async fn returns_text_and_visual_content() {
        let path =
            std::env::temp_dir().join(format!("programmer_read_image_{}.png", std::process::id()));
        tokio::fs::write(&path, png()).await.unwrap();
        let arguments = serde_json::json!({ "path": path }).to_string();

        let FunctionCallOutput::Content(content) = run(&arguments).await.unwrap() else {
            panic!("expected multimodal output");
        };
        assert!(matches!(
            &content[0],
            InputContent::InputText(text) if text.text.contains("2x1")
        ));
        assert!(matches!(
            &content[1],
            InputContent::InputImage(image)
                if image.image_url.as_deref().is_some_and(|url| url.starts_with("data:image/png;base64,"))
        ));

        tokio::fs::remove_file(path).await.ok();
    }

    #[tokio::test]
    async fn rejects_non_images() {
        let path =
            std::env::temp_dir().join(format!("programmer_read_image_{}.txt", std::process::id()));
        tokio::fs::write(&path, b"not an image").await.unwrap();
        let arguments = serde_json::json!({ "path": path }).to_string();

        let error = run(&arguments).await.unwrap_err();
        assert!(error.contains("image encoding is not supported"));

        tokio::fs::remove_file(path).await.ok();
    }
}
