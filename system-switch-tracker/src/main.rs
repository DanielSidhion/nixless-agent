use std::{
    env,
    ffi::CString,
    fs::File,
    io::Write,
    os::unix::fs::{chown, OpenOptionsExt},
    path::PathBuf,
    process::exit,
    str::FromStr,
};

use libc::getpwnam;

fn get_user_group_id(user_name: &str) -> std::io::Result<(u32, u32)> {
    let cname = CString::new(user_name)
        .map_err(|_| std::io::Error::other("unable to convert user name to a cstring"))?;
    let pwd = unsafe { getpwnam(cname.as_ptr()) };
    if pwd.is_null() {
        return Err(std::io::Error::other("user not found"));
    }

    let pwd = unsafe { *pwd };

    Ok((pwd.pw_uid, pwd.pw_gid))
}

fn main() {
    let args: Vec<_> = env::args().collect();

    let [_, track_mode] = &args[0..2] else {
        eprintln!(
            "Received wrong number of arguments, was expecting >=4, got {}.",
            args.len()
        );
        exit(1);
    };

    let [track_directory_path, agent_user] = &args[2..4] else {
        eprintln!(
            "Received wrong number of arguments, was expecting >=4, got {}.",
            args.len()
        );
        exit(1);
    };

    // These come from systemd, but are only set in certain cases (e.g. during ExecStopPost).
    let service_result: Option<String> = env::var("SERVICE_RESULT").ok();
    let exit_code: Option<String> = env::var("EXIT_CODE").ok();
    let exit_status: Option<String> = env::var("EXIT_STATUS").ok();

    let track_file_name = match track_mode.as_str() {
        "pre-switch" => "pre_switch",
        "switch-success" => "switch_success",
        "post-switch" => "post_switch",
        _ => {
            eprintln!(
                "Expected the track mode to be one of 'pre-switch', 'switch-success', 'post-switch', but got '{}'.",
                track_mode
            );
            exit(1);
        }
    };

    let track_directory_path = PathBuf::from_str(&track_directory_path)
        .expect("the directory to keep the tracking files can't be read as a path");

    // TODO: check that `track_directory_path` is actually a directory.

    let (user_id, group_id) = get_user_group_id(agent_user)
        .expect("failed to retrieve id of user associated with given user name");

    let file_path = track_directory_path.join(track_file_name);
    let mut file = File::options()
        .mode(0o600)
        .write(true)
        .create_new(true)
        .open(&file_path)
        .expect("couldn't create a new tracking file");

    if track_mode == "post-switch" {
        let contents = format!(
            "{}\n{}\n{}",
            service_result.unwrap(),
            exit_code.unwrap(),
            exit_status.unwrap()
        );
        file.write_all(contents.as_bytes())
            .expect("failed to write contents to tracking file");
        _ = file.flush();
    }

    drop(file);

    chown(file_path, Some(user_id), Some(group_id))
        .expect("failed to set proper owner for the tracking file");
}
