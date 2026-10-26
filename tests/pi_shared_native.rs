//! Controller-only harness with the same headless reader entry as the product.
#[cfg(unix)]
#[path = "support/pi_shared_native_driver.rs"]
mod driver;

fn main() {
    kitter::run_native_adaptation_reader_if_requested();
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if !arguments.iter().any(|argument| argument == "--ignored") {
        println!("controller-only Pi shared native driver: skipped (requires explicit --ignored)");
        return;
    }
    #[cfg(unix)]
    {
        if arguments.iter().any(|argument| {
            !matches!(
                argument.as_str(),
                "--ignored"
                    | "--exact"
                    | "--nocapture"
                    | "--test-threads=1"
                    | "operate_isolated_native_pi_shared_root"
            )
        }) {
            eprintln!("Unsupported native fixture driver argument.");
            std::process::exit(1);
        }
        if let Err(code) = driver::operate_isolated_native_pi_shared_root() {
            eprintln!("Native fixture driver failed: {code}");
            std::process::exit(1);
        }
    }
    #[cfg(not(unix))]
    println!("controller-only Pi shared native driver: unavailable on this platform");
}
