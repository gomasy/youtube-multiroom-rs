use std::process::Command;

fn main() {
    // Docker ビルドなど .git が無い環境では、ホスト側で求めた値を
    // ARG→ENV 経由で受け取る。環境変数が無ければ git にフォールバックする。
    let hash = env_or_git("GIT_HASH", &["rev-parse", "--short", "HEAD"]);
    println!("cargo:rustc-env=GIT_HASH={hash}");

    let date = env_or_git("BUILD_DATE", &["log", "-1", "--format=%cs"]);
    println!("cargo:rustc-env=BUILD_DATE={date}");

    println!("cargo:rerun-if-env-changed=GIT_HASH");
    println!("cargo:rerun-if-env-changed=BUILD_DATE");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
}

fn env_or_git(var: &str, git_args: &[&str]) -> String {
    non_empty(std::env::var(var).ok())
        .or_else(|| git(git_args))
        .unwrap_or_else(|| "unknown".to_string())
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
        .and_then(|s| non_empty(Some(s)))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
