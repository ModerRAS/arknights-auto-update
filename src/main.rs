use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    match arknights_auto_update::parse_args(env::args().skip(1)) {
        Ok(arknights_auto_update::Command::Help) => {
            print!("{}", arknights_auto_update::help());
            ExitCode::SUCCESS
        }
        Ok(arknights_auto_update::Command::Version) => {
            println!("{}", arknights_auto_update::version_line());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("错误: {err}");
            eprint!("{}", arknights_auto_update::help());
            ExitCode::from(2)
        }
    }
}
