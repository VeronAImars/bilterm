use std::fs;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::buffer::{Pos, TextBuffer};
use super::*;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}
fn type_str(e: &mut EditorState, s: &str) {
    for c in s.chars() {
        e.handle_key(if c == '\n' { key(KeyCode::Enter) } else { key(KeyCode::Char(c)) });
    }
}

#[test]
fn buffer_insert_delete_and_multiline_edits() {
    let mut b = TextBuffer::from_text("héllo\nworld");
    b.cursor = Pos { line: 0, col: 2 };
    b.insert("X\nY");
    assert_eq!(b.text(), "héX\nYllo\nworld");
    assert_eq!(b.cursor, Pos { line: 1, col: 1 });
    b.backspace();
    b.backspace();
    b.backspace();
    assert_eq!(b.text(), "héllo\nworld");
    b.cursor = b.end_pos();
    b.delete_forward();
    assert_eq!(b.text(), "héllo\nworld");
}

#[test]
fn undo_redo_roundtrip_and_typing_coalesces() {
    let mut b = TextBuffer::from_text("");
    for c in "hello".chars() {
        b.insert(&c.to_string());
    }
    b.insert(" ");
    b.insert("w");
    assert_eq!(b.text(), "hello w");
    assert!(b.undo());
    assert_eq!(b.text(), "hello ");
    assert!(b.undo());
    assert_eq!(b.text(), "hello");
    assert!(b.undo());
    assert_eq!(b.text(), "");
    assert!(!b.undo());
    assert!(b.redo() && b.redo() && b.redo());
    assert_eq!(b.text(), "hello w");
    b.insert("!");
    assert!(!b.can_redo(), "a new edit clears redo");
}

#[test]
fn undo_restores_deleted_selection_and_multiline() {
    let mut b = TextBuffer::from_text("a\nbcd\ne");
    b.anchor = Some(Pos { line: 0, col: 1 });
    b.cursor = Pos { line: 2, col: 0 };
    assert_eq!(b.selected_text().as_deref(), Some("\nbcd\n"));
    b.delete_selection();
    assert_eq!(b.text(), "ae");
    b.undo();
    assert_eq!(b.text(), "a\nbcd\ne");
    b.redo();
    assert_eq!(b.text(), "ae");
}

#[test]
fn cut_without_selection_takes_the_line() {
    let mut b = TextBuffer::from_text("one\ntwo\nthree");
    b.cursor = Pos { line: 1, col: 1 };
    assert_eq!(b.cut(), "two\n");
    assert_eq!(b.text(), "one\nthree");
    b.cursor = Pos { line: 1, col: 0 };
    assert_eq!(b.cut(), "\nthree");
    assert_eq!(b.text(), "one");
}

#[test]
fn find_wraps_and_is_case_insensitive() {
    let b = TextBuffer::from_text("Foo bar\nbaz FOO");
    let (s, e) = b.find("foo", Pos { line: 0, col: 1 }, true).unwrap();
    assert_eq!((s, e), (Pos { line: 1, col: 4 }, Pos { line: 1, col: 7 }));
    let (s, _) = b.find("foo", Pos { line: 1, col: 5 }, true).unwrap();
    assert_eq!(s, Pos { line: 0, col: 0 }, "wraps");
    let (s, _) = b.find("foo", Pos { line: 1, col: 4 }, false).unwrap();
    assert_eq!(s, Pos { line: 0, col: 0 });
    assert!(b.find("nope", Pos::default(), true).is_none());
}

#[test]
fn dirty_tracks_save_and_undo_is_still_dirty() {
    let mut b = TextBuffer::from_text("a");
    assert!(!b.is_dirty());
    b.insert("b");
    assert!(b.is_dirty());
    b.mark_saved();
    assert!(!b.is_dirty());
    b.undo();
    assert!(b.is_dirty());
}

#[test]
fn io_refuses_directories_binary_invalid_utf8_and_huge() {
    let d = tempfile::tempdir().unwrap();
    assert!(io::load(d.path()).unwrap_err().contains("not a regular file"));
    let bin = d.path().join("b.bin");
    fs::write(&bin, b"ab\0cd").unwrap();
    assert!(io::load(&bin).unwrap_err().contains("binary"));
    let bad = d.path().join("bad.txt");
    fs::write(&bad, [0xff, 0xfe, b'a']).unwrap();
    assert!(io::load(&bad).unwrap_err().contains("UTF-8"));
    let big = d.path().join("big.txt");
    fs::write(&big, vec![b'a'; (io::MAX_FILE_BYTES + 1) as usize]).unwrap();
    assert!(io::load(&big).unwrap_err().contains("larger"));
    assert!(io::load(&d.path().join("missing.md")).unwrap().is_none());
    assert!(io::save(d.path(), "x", false, None, true).is_err(), "directory target");
}

#[cfg(unix)]
#[test]
fn io_refuses_fifo_and_follows_symlink_to_regular_file() {
    let d = tempfile::tempdir().unwrap();
    let fifo = d.path().join("f");
    let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert!(io::load(&fifo).unwrap_err().contains("not a regular file"));
    assert!(io::save(&fifo, "x", false, None, true).is_err());
    let real = d.path().join("real.txt");
    fs::write(&real, "old").unwrap();
    let link = d.path().join("link.txt");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    io::save(&link, "new", false, None, false).unwrap();
    assert_eq!(fs::read_to_string(&real).unwrap(), "new");
    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "link kept");
}

#[test]
fn save_as_needs_force_to_clobber_and_new_files_work() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("a.md");
    let mut e = EditorState::new_file();
    e.buf.insert("hello");
    assert!(e.save(false).is_err(), "no name yet");
    e.save_as(&p, false).unwrap();
    assert_eq!(fs::read_to_string(&p).unwrap(), "hello\n");
    assert!(!e.is_dirty());
    let mut other = EditorState::new_file();
    other.buf.insert("zzz");
    assert!(matches!(other.save_as(&p, false), Err(SaveError::NeedsConfirm(_))));
    assert_eq!(fs::read_to_string(&p).unwrap(), "hello\n", "untouched");
    other.save_as(&p, true).unwrap();
    assert_eq!(fs::read_to_string(&p).unwrap(), "zzz\n");
}

#[test]
fn save_detects_external_change_and_preserves_crlf_and_mode() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("c.txt");
    fs::write(&p, "a\r\nb\r\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
    }
    let mut e = EditorState::open_file(&p).unwrap();
    assert_eq!(e.buf.text(), "a\nb");
    e.buf.insert("X");
    e.save(false).unwrap();
    assert_eq!(fs::read(&p).unwrap(), b"Xa\r\nb\r\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o640);
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(&p, "changed elsewhere, longer").unwrap();
    e.buf.insert("Y");
    assert!(matches!(e.save(false), Err(SaveError::NeedsConfirm(_))));
    assert_eq!(fs::read_to_string(&p).unwrap(), "changed elsewhere, longer");
    e.save(true).unwrap();
}

#[test]
fn key_flow_type_save_overwrite_confirm_and_markdown_detect() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("n.md");
    fs::write(&p, "existing").unwrap();
    let mut e = EditorState::new_file();
    e.cwd = d.path().to_path_buf();
    type_str(&mut e, "hi\nthere");
    assert_eq!(e.buf.text(), "hi\nthere");
    e.handle_key(ctrl('s')); // no path: prompts for save-as
    assert!(matches!(e.mode, Mode::SaveAs(_)));
    type_str(&mut e, "n.md");
    e.handle_key(key(KeyCode::Enter));
    assert!(matches!(e.mode, Mode::ConfirmOverwrite(_)), "existing file asks first");
    assert_eq!(fs::read_to_string(&p).unwrap(), "existing");
    e.handle_key(key(KeyCode::Char('n')));
    assert_eq!(fs::read_to_string(&p).unwrap(), "existing");
    e.handle_key(ctrl('o'));
    type_str(&mut e, "n.md");
    e.handle_key(key(KeyCode::Enter));
    e.handle_key(key(KeyCode::Char('y')));
    assert_eq!(fs::read_to_string(&p).unwrap(), "hi\nthere\n");
    assert!(e.markdown && !e.is_dirty());
}

#[test]
fn keys_navigation_selection_undo_find_and_passthrough() {
    let mut e = EditorState::new_file();
    type_str(&mut e, "one two\nthree");
    e.handle_key(ctrl('z'));
    assert_eq!(e.buf.text(), "one two\nthre".replace("thre", "") + "", "undo drops the last typed word run");
    e.handle_key(ctrl('y'));
    assert_eq!(e.buf.text(), "one two\nthree");
    e.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL));
    e.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    e.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    assert_eq!(e.buf.selected_text().as_deref(), Some("on"));
    e.handle_key(key(KeyCode::Char('X')));
    assert_eq!(e.buf.line(0), "Xe two");
    e.handle_key(ctrl('f'));
    type_str(&mut e, "thr");
    e.handle_key(key(KeyCode::Enter));
    assert_eq!(e.buf.selected_text().as_deref(), Some("thr"));
    e.handle_key(key(KeyCode::Esc));
    assert!(matches!(e.mode, Mode::Edit));
    // Alt chords, Ctrl+Alt chords and function keys stay with the window manager.
    assert!(!e.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT)));
    assert!(!e.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL | KeyModifiers::ALT)));
    assert!(!e.handle_key(key(KeyCode::F(5))));
}

#[test]
fn enter_auto_indents_and_goto_and_wrap_toggle() {
    let mut e = EditorState::new_file();
    type_str(&mut e, "  - item\nnext");
    assert_eq!(e.buf.line(1), "  next");
    e.handle_key(ctrl('g'));
    type_str(&mut e, "1");
    e.handle_key(key(KeyCode::Enter));
    assert_eq!(e.buf.cursor.line, 0);
    assert!(e.wrap);
    e.handle_key(ctrl('l'));
    assert!(!e.wrap);
}

#[test]
fn render_paints_text_status_dirty_and_markdown_colour() {
    let mut e = EditorState::new_file();
    type_str(&mut e, "# Title\nplain **bold**");
    let out = e.render(6, 30);
    assert!(out.contains("Title") && out.contains("plain"));
    assert!(out.contains("1;36"), "heading style");
    assert!(out.contains("Ln 2/2"), "{out}");
    assert!(out.contains("\x1b[4;"), "status row is row 6");
    assert!(out.contains("~"), "empty rows are marked");
}

#[test]
fn wrapping_scroll_keeps_cursor_visible_and_nowrap_scrolls_horizontally() {
    let mut e = EditorState::new_file();
    e.buf.insert(&"abcdefghij".repeat(5));
    let out = e.render(4, 10);
    assert!(out.contains("abcdefghij"));
    assert!(e.status_json().contains("\"wrap\":true"));
    e.set_wrap(Some(false));
    let out = e.render(4, 10);
    assert!(out.contains("fghij"), "scrolled to the cursor at the end: {out}");
    let mut e = EditorState::new_file();
    e.buf.insert(&(0..50).map(|i| format!("L{i}\n")).collect::<String>());
    e.goto_line(1);
    e.render(5, 20);
    e.goto_line(40);
    let out = e.render(5, 20);
    assert!(out.contains("L39") && !out.contains("L0\x1b"), "{out}");
}

#[test]
fn click_and_drag_position_cursor_and_select() {
    let mut e = EditorState::new_file();
    e.buf.insert("hello\nworld");
    e.render(6, 20);
    e.click(2, 3);
    assert_eq!(e.buf.cursor, Pos { line: 1, col: 3 });
    e.click(1, 1);
    e.drag(1, 4);
    assert_eq!(e.buf.selected_text().as_deref(), Some("ell"));
    e.scroll(1);
}

#[test]
fn scratch_autosaves_privately_restores_and_promotes() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("scratch");
    let f = io::scratch_file(&dir, "scratch").unwrap();
    let mut s = EditorState::open_scratch("scratch", f.clone());
    assert!(s.is_scratch() && !s.is_dirty());
    assert!(!s.autosave_pending());
    s.buf.insert("remember me");
    assert!(s.autosave_pending());
    s.autosave().unwrap();
    assert!(!s.autosave_pending());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    }
    let again = EditorState::open_scratch("scratch", f.clone());
    assert_eq!(again.buf.text(), "remember me");
    let mut s = again;
    let target = d.path().join("kept.md");
    s.save_as(&target, false).unwrap();
    assert!(!s.is_scratch() && !f.exists(), "promotion removes the scratch file");
    assert_eq!(fs::read_to_string(&target).unwrap(), "remember me\n");
}

#[test]
fn scratch_names_are_validated() {
    let d = tempfile::tempdir().unwrap();
    assert!(io::scratch_file(d.path(), "../x").is_err());
    assert!(io::scratch_file(d.path(), "a/b").is_err());
    assert!(io::scratch_file(d.path(), "ok-1_2").is_ok());
}

#[test]
fn op_parse_and_colon_form_roundtrip() {
    let all = [
        ":edit", ":edit /tmp/a b.md", ":scratch", ":scratch notes", ":editor save", ":editor save --force",
        ":editor saveas /tmp/x.md", ":editor saveas /tmp/x.md --force", ":editor wrap", ":editor wrap on", ":editor wrap off",
        ":editor find two words", ":editor findprev x", ":editor undo", ":editor redo", ":editor goto 7", ":editor revert",
        ":editor insert hello world", ":editor text", ":editor status", ":editor close", ":editor close --force",
        ":editor menu open", ":editor menu open file", ":editor menu close", ":editor menu do save-as",
    ];
    for c in all {
        let rest = c.trim_start_matches(':');
        let parts: Vec<&str> = rest.split_whitespace().collect();
        let op = EditorOp::parse(parts[0], &parts).unwrap_or_else(|| panic!("{c}")).unwrap_or_else(|e| panic!("{c}: {e}"));
        assert_eq!(op.colon_form(), c);
    }
    assert!(EditorOp::parse("editor", &["editor", "bogus"]).unwrap().is_err());
    assert!(EditorOp::parse("editor", &["editor", "goto", "x"]).unwrap().is_err());
    assert!(EditorOp::parse("nope", &["nope"]).is_none());
}
