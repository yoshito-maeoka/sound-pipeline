fn main() {
    if let Err(e) = soundpipe::run(soundpipe::Registry::with_builtins()) {
        eprintln!("sp: {e:#}");
        std::process::exit(1);
    }
}
