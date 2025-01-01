use shlex::Shlex;
#[allow(unused_imports)]
use std::io::{self, Write};
use std::{
    env,
    fs::{File, OpenOptions},
    io::Error,
    path::{Path, PathBuf},
    process::{self, Command},
};

const BUILTINS: [&str; 4] = ["echo", "type", "pwd", "exit"];
const REDIR_WRITE_PATTERNS: [&str; 3] = [">", "1>", "2>"];
const REDIR_APPEND_PATTERNS: [&str; 3] = [">>", "1>>", "2>>"];

fn main() {
    let path = std::env::var("PATH").unwrap();

    loop {
        let mut stdout_buffer: Box<dyn Write> = Box::new(io::stdout());
        let mut stderr_buffer: Box<dyn Write> = Box::new(io::stderr());

        write_to_buffer("$ ", &mut stdout_buffer);

        // Wait for user input
        let stdin = io::stdin();
        let mut input = String::new();
        stdin.read_line(&mut input).unwrap();
        let trimmed_input = input.trim();

        if trimmed_input.is_empty() {
            // noop
            continue;
        }

        let posix_friendly_input: Vec<String> = Shlex::new(trimmed_input).collect();
        let mut cmds: Vec<&str> = posix_friendly_input.iter().map(|v| v.as_str()).collect();

        if let Err(err) = parse_redirection(&mut cmds, &mut stdout_buffer, &mut stderr_buffer) {
            println!("Failed to parse redirection: {}", err);
            continue;
        }

        match cmds[..] {
            ["exit"] => process::exit(0),
            ["exit", code] => process::exit(code.parse::<i32>().unwrap()),
            ["pwd", ..] => pwd_cmd(cmds, &mut stdout_buffer),
            ["type", ..] => type_cmd(cmds[1..].to_vec(), &path, &mut stdout_buffer),
            ["echo", ..] => echo_cmd(cmds[1..].to_vec(), &mut stdout_buffer),
            ["cd", ..] => cd_cmd(cmds[1..].to_vec(), &mut stdout_buffer),
            _ => try_external_cmd(&path, cmds, &mut stdout_buffer, &mut stderr_buffer),
        }
    }
}

fn echo_cmd(echo_strs: Vec<&str>, stdout_buffer: &mut Box<dyn Write>) {
    writeln_to_buffer(&format!("{}", echo_strs.join(" ")), stdout_buffer);
}

fn pwd_cmd(cmds: Vec<&str>, stdout_buffer: &mut Box<dyn Write>) {
    if cmds.len() > 1 {
        writeln_to_buffer("too many arguments", stdout_buffer);
        return;
    }

    let current_dir = env::current_dir().unwrap();
    writeln_to_buffer(&format!("{}", current_dir.display()), stdout_buffer);
}

fn cd_cmd(cmds: Vec<&str>, stdout_buffer: &mut Box<dyn Write>) {
    if cmds.len() > 1 {
        writeln_to_buffer("too many arguments", stdout_buffer);
        return;
    }

    if cmds.len() == 0 || cmds[0].starts_with("~") {
        // cd <blank> changes wd to home
        env::set_current_dir(env::var("HOME").unwrap()).unwrap();
        return;
    }

    let path = if cmds[0].starts_with(".") {
        // relative path
        format!("{}/{}", env::current_dir().unwrap().display(), cmds[0])
    } else {
        // absolute path expected
        cmds[0].to_owned()
    };

    let path = Path::new(&path);
    if let Err(_) = env::set_current_dir(path) {
        writeln_to_buffer(
            &format!("cd: {}: No such file or directory", path.display()),
            stdout_buffer,
        );
    }
}

fn type_cmd(type_strs: Vec<&str>, env_path: &str, stdout_buffer: &mut Box<dyn Write>) {
    if type_strs.len() != 1 {
        writeln_to_buffer(
            &format!(
                "incorrect number of args for type command, expected 1, got {}",
                type_strs.len()
            ),
            stdout_buffer,
        );
        return;
    }
    let cmd = &type_strs[0];

    if BUILTINS.contains(cmd) {
        writeln_to_buffer(&format!("{} is a shell builtin", cmd), stdout_buffer);
    } else if let Some(external_cmd) = find_external_cmd(env_path, cmd) {
        writeln_to_buffer(
            &format!("{} is {}", cmd, external_cmd.display()),
            stdout_buffer,
        );
    } else {
        writeln_to_buffer(&format!("{}: not found", cmd), stdout_buffer);
    }
}

fn find_external_cmd(env_path: &str, cmd: &str) -> Option<PathBuf> {
    let path_dirs = &mut env::split_paths(env_path);

    if let Some(path) = path_dirs.find(|path| path.join(cmd).is_file()) {
        return Some(path.join(cmd));
    }
    None
}

fn try_external_cmd(
    env_path: &str,
    cmds: Vec<&str>,
    stdout_buffer: &mut Box<dyn Write>,
    stderr_buffer: &mut Box<dyn Write>,
) {
    let cmd = cmds.first().unwrap(); // panicking here is ok if there's no first elem, since it should've been caught in the main

    if let Some(_) = find_external_cmd(env_path, cmd) {
        let output = Command::new(cmd)
            .args(&cmds[1..])
            .output()
            .expect(format!("failed to execute: {}", cmds.join(" ")).as_str());
        // redirect output to stdout_buffer to handle potential redirections
        // using from_utf8_lossy to handle potential invalid characters
        write_to_buffer(&String::from_utf8_lossy(&output.stdout), stdout_buffer);
        write_to_buffer(&String::from_utf8_lossy(&output.stderr), stderr_buffer);
    } else {
        cmd_not_found(cmd, stdout_buffer);
    }
}

fn parse_redirection<'a>(
    cmds: &mut Vec<&str>,
    stdout_buffer: &mut Box<dyn Write>,
    stderr_buffer: &mut Box<dyn Write>,
) -> Result<(), &'a str> {
    for i in 0..cmds.len() {
        if REDIR_WRITE_PATTERNS.contains(&cmds[i]) {
            apply_redirection(false, i, cmds, stdout_buffer, stderr_buffer)?;
            break;
        }

        if REDIR_APPEND_PATTERNS.contains(&cmds[i]) {
            apply_redirection(true, i, cmds, stdout_buffer, stderr_buffer)?;
            break;
        }
    }
    Ok(())
}

fn apply_redirection<'a>(
    is_append: bool,
    curr_idx: usize,
    cmds: &mut Vec<&str>,
    stdout_buffer: &mut Box<dyn Write>,
    stderr_buffer: &mut Box<dyn Write>,
) -> Result<(), &'a str> {
    // expected format of cmds: [..., "redirection_source", ">>", "redirection_target", ...]
    // ensure redirection target exists
    if curr_idx + 1 >= cmds.len() {
        return Err("Redirection target missing");
    }

    let redir_target = Path::new(cmds[curr_idx + 1]);
    // assert target can be accessed (parent dir exists)
    if !redir_target.parent().unwrap().exists() {
        return Err("Redirection target doesn't exist");
    }

    let redir_buffer: Result<File, Error>;

    if is_append {
        redir_buffer = OpenOptions::new()
            .create(true)
            .append(true)
            .open(redir_target);
    } else {
        redir_buffer = OpenOptions::new()
            .create(true)
            .write(true)
            .open(redir_target);
    }

    if let Err(_) = redir_buffer {
        return Err("Failed to open redirection target for writing");
    }

    if cmds[curr_idx].contains("2") {
        *stderr_buffer = Box::new(redir_buffer.unwrap());
    } else {
        *stdout_buffer = Box::new(redir_buffer.unwrap());
    }

    cmds.drain(curr_idx..=curr_idx + 1);
    Ok(())
}

// using Box<dyn Write> since the function should accept both File and Stdout
fn write_to_buffer(message: &str, buffer: &mut Box<dyn Write>) {
    write!(buffer, "{}", message).unwrap();
    buffer.flush().unwrap();
}

fn writeln_to_buffer(message: &str, buffer: &mut Box<dyn Write>) {
    writeln!(buffer, "{}", message).unwrap();
    buffer.flush().unwrap();
}

fn cmd_not_found(cmd: &str, stdout_buffer: &mut Box<dyn Write>) {
    writeln_to_buffer(&format!("{}: command not found", cmd), stdout_buffer);
}
