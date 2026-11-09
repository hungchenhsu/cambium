use cambium_cli::{Environment, Io, run};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let (mut stdin, mut stdout, mut stderr) =
        (std::io::stdin(), std::io::stdout(), std::io::stderr());
    let status = run(
        std::env::args_os(),
        Environment::from_process(),
        Io {
            stdin: &mut stdin,
            stdout: &mut stdout,
            stderr: &mut stderr,
        },
    )
    .await;
    std::process::exit(status);
}
