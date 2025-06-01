use std::env;
use std::io::Read;
use std::process;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use lazy_static::lazy_static;
use nix::unistd::Pid;
use nix::sys::signal::{kill,Signal};
use std::time::Duration;
use timeout_readwrite::TimeoutReader;
use regex::Regex;
use cursive::{Cursive,XY};
use cursive::event::Event;
use cursive::align::HAlign;
use cursive::views::{ResizedView, Dialog, LinearLayout, TextContent, TextView, Panel};
use cursive::traits::*;
use rasciigraph::{plot, Config};
use std::fs::OpenOptions;
use std::io::prelude::*;
#[allow(unreachable_code)]
#[allow(unused_assignments)]
#[allow(unused_variables)]

fn has_iperf3() -> bool {
   return Command::new("which").arg("iperf3").stdout(Stdio::null()).stderr(Stdio::null()).status().expect("Could not run 'which iperf3'").success();
}

lazy_static! {
    static ref IPERF3_PID: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
    static ref SCREEN_SIZE: Arc<Mutex<Option<XY<usize>>>> = Arc::new(Mutex::new(None));
}

fn save_pid(pid_in: u32) {
    let mut pid_opt = IPERF3_PID.lock().unwrap();
    let pid_u32:u32 = pid_in;
    let pid_i32:i32 = pid_u32.try_into().unwrap();
    *pid_opt = Some(pid_i32);
}

fn kill_pid() {
    let pid_opt = IPERF3_PID.lock().unwrap();
    if pid_opt.is_some() {
        let pid_struct = Pid::from_raw(pid_opt.unwrap());
        kill(pid_struct, Signal::SIGKILL).expect("Could not kill child process");
    }
}

fn save_screen_size(ss_in: XY<usize>) {
    let mut ss_opt = SCREEN_SIZE.lock().unwrap();
    *ss_opt = Some(ss_in);
}

fn get_screen_size() -> (u32, u32) {
    let screen_width: u32;
    let screen_height: u32;
    let ss_opt = SCREEN_SIZE.lock().unwrap();
    if ss_opt.is_some() {
        screen_width = ss_opt.unwrap().x.try_into().unwrap();
        screen_height = ss_opt.unwrap().y.try_into().unwrap();
    }
    else {  // Defaults
        screen_width = 80;
        screen_height = 24;
    }
    return (screen_width, screen_height);
}

fn average(numbers: &Vec::<f64>) -> f64 {
    let sum:f64  = numbers.iter().sum();
    let count = numbers.len() as f64;
    return sum / count;
}

fn scale(units: &mut String, bitrates_in: &Vec::<f64>) -> Vec::<f64> {
    let factor = 1000.0;
    let factor_squared = factor * factor;
    let factor_cubed = factor_squared * factor;
    let factor_reciprocal = 1.0 / factor;
    let factor_reciprocal_squared = factor_reciprocal * factor_reciprocal;

    let mut bitrates_scaled = bitrates_in.clone();
    let average = average(&bitrates_scaled);
    if average > factor_cubed {
        for item in &mut bitrates_scaled {
            *item = *item / factor_cubed;
        }
        *units = "Pbits".to_string();
    }
    else if average > factor_squared {
        for item in &mut bitrates_scaled {
           *item = *item / factor_squared;
        }
        *units = "Tbits".to_string();
    }
    else if average > factor {
        for item in &mut bitrates_scaled {
           *item = *item / factor;
        }
        *units = "Gbits".to_string();
    }
    else if average < factor_reciprocal_squared {
        for item in &mut bitrates_scaled {
            *item = *item * factor_reciprocal_squared;
        }
        *units = "bits".to_string();
    }
    else if average < factor_reciprocal {
        for item in &mut bitrates_scaled {
            *item = *item * factor;
        }
        *units = "Kbits".to_string();
    }
    return bitrates_scaled;
}

fn left_pad(str_in: String, n: usize) -> String {
    let mut s = str_in;
    while s.len() < n {
        s = " ".to_owned() + &s;
    }
    return s;
}

fn replace_at_start(original: &str, replacement: &str) -> String {
    let original_len = original.len();
    let mut n = replacement.len();
    n = n.min(original_len);
    return replacement.to_owned() + &original[n..];
}

#[allow(dead_code)]
fn save_to_file(filename: String, content: &String) {
    let mut file = OpenOptions::new().append(true).create(true).open(filename).unwrap();
    write!(&mut file, "{}", content).expect("Could not write to file");
}

fn background_graph(content_graph: TextContent, server: String) {
    let result = Command::new("iperf3")
       .arg("--forceflush") // Don't buffer between lines
       .arg("--interval").arg("1") // Every second
       .arg("--time").arg("0") // Run forever
       .arg("--format").arg("m")   // In megabits
       .arg("--client") // We are a client
       .arg(server.clone())
       .stdout(Stdio::piped())
       .stderr(Stdio::piped())
       .spawn();
    if result.is_err() {
       content_graph.set_content("Could not run iperf3 - is it installed?");
       return;
    }
    let child = result.unwrap();

    save_pid(child.id());

    //
    // stderr
    //
  
    {
        let stderr_msg = format!("Checking connection to {}...", server);
        content_graph.set_content(stderr_msg);
   
        let stderr_result = child.stderr;

        if stderr_result.is_none() {
            content_graph.set_content("Could not get stderr from iperf3");
            return;
        }
        let stderr = stderr_result.unwrap();

        let mut stderr_data = String::new();
        let mut stderr_rdr = TimeoutReader::new(stderr, Duration::new(5, 0));
        let _ = stderr_rdr.read_to_string(&mut stderr_data);
        if !stderr_data.is_empty() {
            let stderr_err = format!("{}... please quit this app", stderr_data);
            content_graph.set_content(stderr_err);
            return;
        }
    }

    //
    // stdout
    //

    let stdout_result = child.stdout;

    if stdout_result.is_none() {
        content_graph.set_content("Could not get output from iperf3");
        return;
    }
    let stdout = stdout_result.unwrap();

    // content_graph.set_content("iperf3 started, waiting for reply");

    // let lines = Vec::<String>::new();
    let re_main = Regex::new("\\[([^\\]]+)\\]\\s(.*)$").unwrap();
    // let re_time = Regex::new("([\\d\\.]+)-([\\d\\.]+)").unwrap();
    let re_bitrate = Regex::new("([\\d\\.]+)\\s(\\w+)/sec").unwrap();

    let mut bitrates = Vec::<f64>::new();
    // let mut times = Vec::<f64>::new();
    
    let mut byte_line = Vec::new();
    for byte_result in stdout.bytes() {
        let byte = byte_result.unwrap();
        if byte == b'\n' {
            let line = String::from_utf8_lossy(&byte_line).to_string();
            byte_line.clear();

            // let mut id = "";
            let mut remainder = "";
            // let mut second = "";
            let mut bitrate: String = "".to_string();
            let mut units: String = "".to_string();
            let caps_main = re_main.captures(&line);
            if caps_main.is_some() {
                let c = caps_main.unwrap();
                // id = &c.get(1).unwrap().as_str().trim();
                remainder = &c.get(2).unwrap().as_str().trim();
            }

            if line.contains("- - -") {
                // End
            }
            else if remainder.contains("Interval") {
                // Start or end
            }
            else if !remainder.is_empty() {
                /*
                let caps_time = re_time.captures(&remainder);
                if caps_time.is_some() {
                    let c = caps_time.unwrap();
                    second = &c.get(1).unwrap().as_str().trim();
                }
                */

                let caps_bitrate = re_bitrate.captures(&remainder);
                if caps_bitrate.is_some() {
                    let c = caps_bitrate.unwrap();
                    bitrate = c.get(1).unwrap().as_str().trim().to_string();
                    units = c.get(2).unwrap().as_str().trim().to_string();
                }
            }

            if !bitrate.is_empty() {
                let (screen_width, screen_height) = get_screen_size();

                let bitrate_f64:f64 = bitrate.parse().unwrap();
                bitrates.push(bitrate_f64.to_owned());
                // let second_f64:f64 = second.parse().unwrap();
                // times.push(second_f64.to_owned());
                let graph_width = screen_width - 10;
                let graph_height = screen_height - 8;
                if bitrates.len() > graph_width as usize {
                    bitrates.remove(0);
                    // times.remove(0);
                }

                // Scale
                let bitrates_scaled = scale(&mut units, &bitrates);

                //
                // Plot
                //

                {
                    let config = Config::default().with_width(graph_width).with_height(graph_height);
                    let content1 = plot(bitrates_scaled.clone(), config);
                    let units_pad = left_pad(units.to_string(), 6);
                    let content2 = replace_at_start(&content1, &units_pad);
    
                    content_graph.set_content(&content2);

                    // let content3 = content2 + "\n";
                    // save_to_file("graph.txt".to_string(), &content3);
                }
            }
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
    if !has_iperf3() {
        eprintln!("Please install `iperf3`");
        process::exit(1);
    }

    let args: Vec<String> = env::args().collect();
    if args.len() <= 1 {
        eprintln!("Usage: iperf3-tui <iperf3-server>");
        process::exit(1);
    }
    let server = args[1].clone();

    let mut siv = cursive::default();
    let content_graph = TextContent::new("Starting...");
    let tv3 = TextView::new_with_content(content_graph.clone())
       .no_wrap()
       .with_name("tv3");

    let box3 = ResizedView::with_full_screen(tv3);
    let pan3 = Panel::new(box3).title(&server);

    let tv4 = TextView::new("Press 'q' to quit");
    let box4 = ResizedView::with_min_height(1, tv4);

    siv.add_layer(
       Dialog::around(
           LinearLayout::vertical()
               .child(pan3)
               .child(box4)
       )
       .title("iperf3-tui")
       .h_align(HAlign::Center),
    );

    siv.add_global_callback('q', on_quit);

    std::thread::spawn(move || { background_graph(content_graph, server) });

    siv.set_fps(1);

    // Add a global callback that will be called when the layout is done
    siv.add_global_callback(Event::Refresh, |s| {
        save_screen_size(s.screen_size());
    });

    siv.run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        if has_iperf3() {
            // TODO
            return;
        }
    }
}
