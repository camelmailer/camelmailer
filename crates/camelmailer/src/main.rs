//! The stock `camelmailer` binary: the CLI from the library crate with no
//! extensions. Custom builds call [`camelmailer::main_with`] instead.

fn main() -> std::process::ExitCode {
    camelmailer::main()
}
