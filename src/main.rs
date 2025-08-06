use std::process::{Command, Stdio};
use std::env;
use std::io::Read;
use std::sync::{Arc, Mutex};
use lazy_static::lazy_static;
use nix::unistd::Pid;
use nix::sys::signal::{kill,Signal};
use cursive::Cursive;
use cursive::align::HAlign;
use cursive::views::{ResizedView, Dialog, LinearLayout, TextContent, TextView, Panel};
use cursive::view::{SizeConstraint, ScrollStrategy};
use cursive::traits::*;
mod wgg;

fn is_root() -> bool {
    return users::get_current_uid() == 0
}

fn get_ifconfig(wg_interface: &str) -> String {
    let msg = format!("Could not run 'ifconfig {}'", wg_interface);
    let output = Command::new("ifconfig").arg(wg_interface).output().expect(&msg);
    return String::from_utf8_lossy(&output.stdout).to_string();
}

fn background_top(wg_interface: String, content_wg: TextContent, content_ifconfig: TextContent) {
    let half_sec = std::time::Duration::from_millis(500);
    loop {
         content_wg.set_content(wgg::get_wgg());
         std::thread::sleep(half_sec);
         content_ifconfig.set_content(get_ifconfig(&wg_interface));
         std::thread::sleep(half_sec);
    }
}

lazy_static! {
    static ref TCP_DUMP_PID: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
}

fn save_pid(pid_in: u32) {
    let mut pid_opt = TCP_DUMP_PID.lock().unwrap();
    let pid_i32:i32 = pid_in.try_into().unwrap();
    *pid_opt = Some(pid_i32);
}

fn kill_pid() {
    let pid_opt = TCP_DUMP_PID.lock().unwrap();
    if pid_opt.is_some() {
        let pid_struct = Pid::from_raw(pid_opt.unwrap());
        kill(pid_struct, Signal::SIGKILL).expect("Could not kill child process");
    }
}

fn vec_to_text(vec: &Vec<String>) -> String {
    let mut txt: String = "".to_owned();
    for line in vec.iter() {
        let bline = &line;
        txt.push_str(bline);
        txt.push_str("\n");
    }
    return txt;
}

fn background_tcpdump(wg_interface: String, content_tcpdump: TextContent) {
    if !is_root() {
        content_tcpdump.set_content("You must be root to run `tcpdump`");
        return;
    }

    let dash_i = format!("-i{}", wg_interface);
    let result = Command::new("tcpdump")
        .arg(dash_i)
        .arg("-l")          // Line buffering (avoids delay)
        .arg("-n")          // No DNS lookups
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    if result.is_err() {
        content_tcpdump.set_content("Could not run tcpdump - permission?");
        return;
    }

    let child = result.unwrap();

    /*
    {   // Save PID
        let mut pid_opt = TCP_DUMP_PID.lock().unwrap();
        let pid_i32:i32 = child.id().try_into().unwrap();
        *pid_opt = Some(pid_i32);
    }
    */
    save_pid(child.id());

    let stdout_result = child.stdout;

    if stdout_result.is_none() {
        content_tcpdump.set_content("Could not get output from tcpdump");
        return;
    }

    let stdout = stdout_result.unwrap();

    let msg = format!("tcpdump start, waiting for {} traffic", wg_interface);
    content_tcpdump.set_content(msg);

    let mut vec = Vec::<String>::new();

    let mut byte_line = Vec::new();
    for byte_result in stdout.bytes() {
        let byte = byte_result.unwrap();
        if byte == b'\n' {
            let line = String::from_utf8_lossy(&byte_line).to_string();
            byte_line.clear();
            vec.push(line);

            if vec.len() > 100 {
                vec.remove(0);
            }

            content_tcpdump.set_content(vec_to_text(&vec));
        }
        else {
            byte_line.push(byte);
        }
    }
}

fn on_quit(siv: &mut Cursive) {
    kill_pid();
    siv.quit();
}

fn main() {
    let mut wg_interface = String::from("wg0");
    let args: Vec<String> = env::args().collect();
    if args.len() > 1 {
        wg_interface = args[1].clone();
    }

    let mut siv = cursive::default();

    let content_wg = TextContent::new("wg...");
    let tv1 = TextView::new_with_content(content_wg.clone())
        .no_wrap()
        .with_name("tv1")
        .scrollable();
    let swidth = SizeConstraint::Full;
    let sheight = SizeConstraint::Full;
    let box1 = ResizedView::new(swidth, sheight, tv1).with_name("box1");
    let pan1 = Panel::new(box1).title("wg show");

    let content_ifconfig = TextContent::new("ifconfig...");
    let tv2 = TextView::new_with_content(content_ifconfig.clone())
        .no_wrap()
        .with_name("tv2")
        .scrollable();
    let box2 = ResizedView::with_max_height(7, tv2);
    let title2 = format!("ifconfig {}", wg_interface);
    let pan2 = Panel::new(box2).title(title2);

    let content_tcpdump = TextContent::new("tcpdump...");
    let mut tv3 = TextView::new_with_content(content_tcpdump.clone())
        .no_wrap()
        .with_name("tv3")
        .scrollable();
    tv3.set_scroll_strategy(ScrollStrategy::StickToBottom);
    let box3 = ResizedView::new(swidth, sheight, tv3);
    let title3 = format!("tcpdump -i{}", wg_interface);
    let pan3 = Panel::new(box3).title(title3);

    let tv4 = TextView::new("Press 'q' to quit");

    siv.add_layer(
        Dialog::around(
            LinearLayout::vertical()
                .child(pan1)
                .child(pan2)
                .child(pan3)
                .child(tv4)
        )
        .title("wireguard monitor")
        .h_align(HAlign::Center),
    );

    siv.add_global_callback('q', on_quit);

    let wg_interface_top = wg_interface.clone();
    let wg_interface_tcpdump = wg_interface.clone();
    std::thread::spawn(move || { background_top(wg_interface_top, content_wg, content_ifconfig) });
    std::thread::spawn(move || { background_tcpdump(wg_interface_tcpdump, content_tcpdump) });

    siv.set_fps(1);

    siv.run();
}
