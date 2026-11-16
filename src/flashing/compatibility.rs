use std::{fs, io::Write as _, path::Path};

use sha2::{Digest as _, Sha256};

const CONSOLE_SHA256: &str = "c3c4ef87d82df9f672138cdc3407e9c8029e50a1a6abe3c255a7b596d0235514";
const CLEANUP_OFFSET: usize = 0x760;
const CLEANUP_ENTRY: &[u8] = &[0x51, 0xe8, 0xda, 0xfd, 0xff, 0xff];
const BATCH_EXIT_OFFSET: usize = 0x3fa8;
const BATCH_EXIT_ENTRY: &[u8] = &[0x0f, 0x8d, 0x37, 0x01, 0x00, 0x00];

fn guarded_image(mut image: Vec<u8>) -> Result<Vec<u8>, String> {
    if format!("{:x}", Sha256::digest(&image)) != CONSOLE_SHA256
        || image.get(CLEANUP_OFFSET..CLEANUP_OFFSET + CLEANUP_ENTRY.len()) != Some(CLEANUP_ENTRY)
        || image.get(BATCH_EXIT_OFFSET..BATCH_EXIT_OFFSET + BATCH_EXIT_ENTRY.len())
            != Some(BATCH_EXIT_ENTRY)
    {
        return Err("Unsupported PhoenixConsole build: cannot safely disable its global adb.exe cleanup. Use the supplied PhoenixConsole 2.1.0 tool.".into());
    }
    image[CLEANUP_OFFSET..CLEANUP_OFFSET + 3].copy_from_slice(&[0x31, 0xc0, 0xc3]);
    image[BATCH_EXIT_OFFSET..BATCH_EXIT_OFFSET + BATCH_EXIT_ENTRY.len()].fill(0x90);
    Ok(image)
}

pub(super) fn prepare_console(tool_dir: &Path) -> Result<tempfile::TempPath, String> {
    let image = fs::read(tool_dir.join("PhoenixConsole.exe")).map_err(|error| error.to_string())?;
    let image = guarded_image(image)?;
    let mut copy = tempfile::Builder::new()
        .prefix(".shelllf-console-")
        .suffix(".exe")
        .tempfile_in(tool_dir)
        .map_err(|error| {
            format!("Cannot create a private console copy in the tool folder: {error}")
        })?;
    copy.write_all(&image)
        .and_then(|_| copy.flush())
        .map_err(|error| error.to_string())?;
    Ok(copy.into_temp_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_verified_console_has_its_global_adb_cleanup_disabled() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("dist/PhoenixConsole_Windows/PhoenixConsole.exe");
        let original = fs::read(path).unwrap();
        let patched = guarded_image(original.clone()).unwrap();
        assert_eq!(&patched[..CLEANUP_OFFSET], &original[..CLEANUP_OFFSET]);
        assert_eq!(
            &patched[CLEANUP_OFFSET..CLEANUP_OFFSET + 3],
            &[0x31, 0xc0, 0xc3]
        );
        assert_eq!(
            &patched[CLEANUP_OFFSET + 3..BATCH_EXIT_OFFSET],
            &original[CLEANUP_OFFSET + 3..BATCH_EXIT_OFFSET]
        );
        assert_eq!(
            &patched[BATCH_EXIT_OFFSET..BATCH_EXIT_OFFSET + 6],
            &[0x90; 6]
        );
        assert_eq!(
            &patched[BATCH_EXIT_OFFSET + 6..],
            &original[BATCH_EXIT_OFFSET + 6..]
        );
        assert!(guarded_image(patched).is_err());
        assert!(guarded_image(vec![]).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn private_console_runs_help_and_leaves_the_original_unchanged() {
        use std::os::windows::process::CommandExt as _;
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("dist/PhoenixConsole_Windows");
        let original = fs::read(directory.join("PhoenixConsole.exe")).unwrap();
        let program = prepare_console(&directory).unwrap();
        let output = std::process::Command::new(&program)
            .current_dir(&directory)
            .arg("-h")
            .creation_flags(0x0800_0000)
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).contains("--no-adb"));
        assert_eq!(
            fs::read(directory.join("PhoenixConsole.exe")).unwrap(),
            original
        );
        let path = program.to_path_buf();
        drop(program);
        assert!(!path.exists());
    }
}
