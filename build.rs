use std::env;
use std::process::{Command, Stdio};


fn stamp_version() {
    const SOURCE_FILE_PATH: &str = "src/main.rs";
    const PLACEHOLDER: &str = "<unknown git revision>";

    let running_git = Command::new("git")
        .arg("rev-parse")
        .arg("--short")
        .arg("HEAD")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to ask git for current revision");
    let revision_output = running_git.wait_with_output()
        .expect("git failed to return current revision")
        .stdout;
    let revision_string = String::from_utf8(revision_output)
        .expect("git revision is not valid UTF-8?!");

    let source_file_contents = std::fs::read_to_string(SOURCE_FILE_PATH)
        .expect("failed to read source file");
    if source_file_contents.find(PLACEHOLDER).is_none() {
        panic!("cannot find placeholder in source file");
    }
    let new_source_file_contents = source_file_contents.replace(PLACEHOLDER, revision_string.trim());
    std::fs::write(SOURCE_FILE_PATH, &new_source_file_contents)
        .expect("failed to write source file");
}


fn main() {
    let want_version_stamp = env::var("PROM_RAD_EXP_CICD")
        .map(|val| val == "1")
        .unwrap_or(false);
    if want_version_stamp {
        stamp_version();
    }
}
