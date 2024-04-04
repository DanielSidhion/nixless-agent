use std::{
    env,
    fs::File,
    io::Write,
    os::unix::fs::{chown, OpenOptionsExt},
    path::PathBuf,
    process::exit,
    str::FromStr,
};

fn get_user_group_id(user_name: &str) -> std::io::Result<(u32, u32)> {
    let passwd = std::fs::read_to_string("/etc/passwd")?;
    let user_entry = passwd.lines().find(|line| line.starts_with(user_name));

    if let Some(user_entry) = user_entry {
        let [_, _, uid, gid, _] = user_entry.splitn(5, ":").collect::<Vec<_>>()[..] else {
            return Err(std::io::Error::other(
                "passwd line didn't follow expected format",
            ));
        };
        let uid = uid
            .parse()
            .expect("passwd line didn't follow expected format");
        let gid = gid
            .parse()
            .expect("passwd line didn't follow expected format");
        Ok((uid, gid))
    } else {
        Err(std::io::Error::other("user not found"))
    }
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

    let service_result: Option<&String>;
    let exit_code: Option<&String>;
    let exit_status: Option<&String>;

    if track_mode == "post-switch" {
        if args.len() != 7 {
            eprintln!(
                "Received wrong number of arguments, was expecting 7, got {}.",
                args.len()
            );
            exit(1);
        }
        service_result = Some(&args[4]);
        exit_code = Some(&args[5]);
        exit_status = Some(&args[6]);
    } else {
        service_result = None;
        exit_code = None;
        exit_status = None;
    }

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
