use super::{KITTY_BLUE_FRAME, KITTY_RED};
use crate::{Terminal, check, ffi};
use std::ptr::from_ref;
use sumie_core::graphics::{ImageSource, PixelFormat};

const GREEN_FRAME: &str = "a=f,f=32,i=7,s=1,v=1;AP8A/w==";
const NEW_FRAME_FULL: &str = "i=7,r=2;ENOSPC: animation frame storage full";

fn set_limit(vt: &Terminal, limit: u64) {
    // SAFETY: The terminal is live; option 16 reads a u64 during the call.
    check(unsafe { ffi::ghostty_terminal_set(vt.raw, 16, from_ref(&limit).cast()) }).unwrap();
}

fn terminal(limit: u64) -> Terminal {
    let mut vt = Terminal::new(6, 3).unwrap();
    vt.resize_pixels(6, 3, [8, 16]).unwrap();
    set_limit(&vt, limit);
    vt
}

fn kitty(vt: &mut Terminal, command: &str, reply: &str) {
    vt.write(format!("\x1b_G{command}\x1b\\").as_bytes());
    let mut actual = Vec::new();
    vt.drain_replies(|bytes| actual.extend_from_slice(bytes));
    if reply.is_empty() {
        assert!(actual.is_empty(), "{command}");
    } else {
        assert_eq!(
            actual,
            format!("\x1b_G{reply}\x1b\\").as_bytes(),
            "{command}"
        );
    }
}

#[test]
fn kitty_root_conversion_validates_and_reserves_before_mutating() {
    const RGB: &[u8] = &[255, 0, 0, 0, 0, 255];
    const COMPOSE: &str = "a=c,i=7,r=1,c=1,w=1,h=1,x=1";
    let mut vt = terminal(6);
    kitty(&mut vt, "a=T,f=24,i=7,s=2,v=1,C=1;/wAAAAD/", "i=7;OK");
    let original = vt.render_snapshot().unwrap();
    let root = &original.images[0];
    assert_eq!(root.image.format, PixelFormat::Rgb);
    for (command, reply) in [
        (
            "a=f,f=32,i=7,s=1,v=1,c=9;AP8A/w==",
            "i=7,r=2;EINVAL: base frame not found",
        ),
        (COMPOSE, "i=7;ENOSPC: animation frame storage full"),
        (
            "a=f,f=32,i=7,s=1,v=1,r=1;AP8A/w==",
            "i=7,r=1;ENOSPC: animation frame storage full",
        ),
    ] {
        kitty(&mut vt, command, reply);
        assert_eq!(
            vt.render_snapshot().unwrap().images.as_slice(),
            std::slice::from_ref(root)
        );
        assert_eq!(vt.pixels(root), Some(RGB));
    }

    set_limit(&vt, 9);
    kitty(&mut vt, "a=t,f=24,i=8,s=1,v=1;AP8A", "i=8;OK");
    kitty(&mut vt, GREEN_FRAME, NEW_FRAME_FULL);
    assert_eq!(vt.pixels(root), Some(RGB));
    kitty(&mut vt, "a=p,i=8,C=1", "i=8;OK");
    kitty(&mut vt, COMPOSE, "i=7;OK");
    let snapshot = vt.render_snapshot().unwrap();
    assert_eq!(snapshot.images.len(), 1);
    let updated = &snapshot.images[0];
    assert_eq!(updated.image.format, PixelFormat::Rgba);
    assert_ne!(updated.image.generation, root.image.generation);
    assert_eq!(
        vt.pixels(updated),
        Some([255, 0, 0, 255, 255, 0, 0, 255].as_slice())
    );
}

#[test]
fn kitty_frame_capacity_is_charged_until_freed() {
    let mut vt = terminal(8);
    kitty(&mut vt, KITTY_RED, "i=7,p=2;OK");
    kitty(&mut vt, GREEN_FRAME, NEW_FRAME_FULL);

    for (remove_last, limit) in [(false, 40), (true, 4)] {
        set_limit(&vt, 512);
        kitty(&mut vt, KITTY_RED, "i=7,p=2;OK");
        kitty(&mut vt, GREEN_FRAME, "i=7,r=2;OK");
        kitty(&mut vt, KITTY_BLUE_FRAME, "i=7,r=3;OK");
        kitty(&mut vt, "a=d,d=f,i=7,r=3", "");
        if remove_last {
            kitty(&mut vt, "a=d,d=f,i=7,r=2", "");
        }
        // 40 bytes fits the root plus one frame record and its pixels,
        // but not an allocation with unused frame slots.
        set_limit(&vt, limit);
        assert_eq!(
            vt.render_snapshot().unwrap().images.len(),
            usize::from(remove_last)
        );
    }
}

#[test]
fn kitty_frame_deletion_preserves_or_refreshes_the_displayed_revision() {
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    let mut vt = terminal(512);
    kitty(&mut vt, KITTY_RED, "i=7,p=2;OK");
    kitty(&mut vt, GREEN_FRAME, "i=7,r=2;OK");
    kitty(&mut vt, KITTY_BLUE_FRAME, "i=7,r=3;OK");
    kitty(&mut vt, "a=a,i=7,c=2,s=1", "");
    let mut viewer = vt.render_snapshot().unwrap().images[0].clone();
    assert_eq!(vt.pixels(&viewer), Some(GREEN.as_slice()));

    for (command, reply, expected, changed) in [
        ("a=d,d=f,i=7,r=2", "", BLUE, true),
        ("a=d,d=f,i=7,r=1", "", BLUE, false),
        (GREEN_FRAME, "i=7,r=2;OK", BLUE, false),
        ("a=a,i=7,c=2,s=1", "", GREEN, true),
        ("a=d,d=f,i=7,r=9", "", BLUE, true),
    ] {
        kitty(&mut vt, command, reply);
        let snapshot = vt.render_snapshot().unwrap();
        let shown = &snapshot.images[0];
        assert_eq!(shown.image.generation != viewer.image.generation, changed);
        assert!(vt.refresh_viewer(&mut viewer).unwrap());
        assert_eq!(&viewer, shown);
        assert_eq!(vt.pixels(&viewer), Some(expected.as_slice()));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn kitty_file_transfers_keep_temporary_files_and_reject_links() {
    use std::{fs, os::unix::fs::symlink, path::PathBuf};

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    let dir = PathBuf::from(format!("/dev/shm/sumie-kitty-{}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let scratch = Scratch(dir);
    let source = scratch.0.join("tty-graphics-protocol");
    let link = scratch.0.join("symlink");
    let hard_link = scratch.0.join("hardlink");
    let pixels = [255, 0, 0];
    fs::write(&source, pixels).unwrap();
    symlink(&source, &link).unwrap();

    let mut vt = terminal(512);
    let mut query = |medium, path: &std::path::Path, reply| {
        let payload = base64_path(path.as_os_str().as_encoded_bytes());
        kitty(
            &mut vt,
            &format!("a=q,t={medium},i=7,f=24,s=1,v=1;{payload}"),
            reply,
        );
    };
    query('t', &source, "i=7;OK");
    assert_eq!(fs::read(&source).unwrap(), pixels);
    query('f', &link, "i=7;EINVAL: invalid data");
    fs::hard_link(&source, &hard_link).unwrap();
    query('f', &hard_link, "i=7;EINVAL: invalid data");
}

#[cfg(target_os = "linux")]
fn base64_path(path: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(path.len().div_ceil(3) * 4);
    for chunk in path.chunks(3) {
        let bytes = [0, 1, 2].map(|i| u32::from(chunk.get(i).copied().unwrap_or(0)));
        let word = bytes[0] << 16 | bytes[1] << 8 | bytes[2];
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            out.push(if i <= chunk.len() {
                char::from(ALPHABET[(word >> shift) as usize & 63])
            } else {
                '='
            });
        }
    }
    out
}
