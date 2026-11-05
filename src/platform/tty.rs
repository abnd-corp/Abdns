//! Terminal POSIX (macOS, Linux, BSD) : saisie d'un secret sans écho.

use std::io::{self, BufRead, IsTerminal};
use std::sync::OnceLock;

const INPUT: libc::c_int = libc::STDIN_FILENO;
/// Signaux qui terminent la saisie (Ctrl-C, Ctrl-\, terminal fermé) : l'écho est remis avant.
const ENDING: [libc::c_int; 4] = [libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGHUP];

/// Réglages du terminal d'avant la saisie, pour le gestionnaire de signal.
static SAVED: OnceLock<libc::termios> = OnceLock::new();

extern "C" fn restore_and_exit(signal: libc::c_int) {
    if let Some(saved) = SAVED.get() {
        // SAFETY: tcsetattr est utilisable dans un gestionnaire de signal (POSIX) ; réglages
        // initialisés avant que le gestionnaire soit posé.
        unsafe { libc::tcsetattr(INPUT, libc::TCSANOW, saved) };
    }
    // SAFETY: _exit est utilisable dans un gestionnaire de signal et ne revient pas.
    unsafe { libc::_exit(128 + signal) }
}

fn set_handlers(handler: libc::sighandler_t) {
    for signal in ENDING {
        // SAFETY: signal valide ; gestionnaire par défaut ou fonction `extern "C"` qui ne fait
        // que des appels permis dans un gestionnaire.
        unsafe { libc::signal(signal, handler) };
    }
}

/// Écho coupé, saut de ligne gardé (ECHONL) ; rend les réglages d'avant.
fn hide_echo() -> io::Result<libc::termios> {
    // SAFETY: structure de données C sans pointeur, remplie par tcgetattr avant toute lecture.
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: descripteur de l'entrée standard, structure de la taille attendue.
    if unsafe { libc::tcgetattr(INPUT, &mut term) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let saved = *SAVED.get_or_init(|| term);
    set_handlers(restore_and_exit as extern "C" fn(libc::c_int) as libc::sighandler_t);
    term.c_lflag = (term.c_lflag & !libc::ECHO) | libc::ECHONL;
    // SAFETY: descripteur de l'entrée standard, réglages lus juste avant.
    if unsafe { libc::tcsetattr(INPUT, libc::TCSANOW, &term) } != 0 {
        let error = io::Error::last_os_error();
        set_handlers(libc::SIG_DFL);
        return Err(error);
    }
    Ok(saved)
}

fn restore(saved: &libc::termios) {
    // SAFETY: descripteur de l'entrée standard, réglages lus par tcgetattr.
    unsafe { libc::tcsetattr(INPUT, libc::TCSANOW, saved) };
    set_handlers(libc::SIG_DFL);
}

/// Une ligne de l'entrée standard, sans son saut de ligne. Dans un terminal, sans écho, et
/// l'écho revient même si la saisie est interrompue (Ctrl-C).
pub fn read_secret_line() -> io::Result<String> {
    let stdin = io::stdin();
    let saved = if stdin.is_terminal() { Some(hide_echo()?) } else { None };
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);
    if let Some(saved) = &saved {
        restore(saved);
    }
    read?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}
