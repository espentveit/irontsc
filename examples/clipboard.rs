//! Prints what is on this machine's clipboard, the way a paste would see it.
//!
//! Useful for checking that a copy on the remote desktop reached this side: `xclip` reads X11's
//! selection, which on a Wayland session is a different space from the one applications use.

fn main() {
    match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
        Ok(text) => println!("{text}"),
        Err(error) => eprintln!("clipboard: {error}"),
    }
}
