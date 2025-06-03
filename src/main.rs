use std::io::Read;
use std::process;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use clap::Parser;
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

fn has_iperf3() -> bool {
   return Command::new("which").arg("iperf3").stdout(Stdio::null()).stderr(Stdio::null()).status().expect("Could not run 'which iperf3'").success();
}

lazy_static! {
    static ref IPERF3_PID: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
    static ref SCREEN_SIZE: Arc<Mutex<Option<XY<usize>>>> = Arc::new(Mutex::new(None));
}

fn save_pid(pid_in: u32) {
    let mut pid_opt = IPERF3_PID.lock().unwrap();
    let pid_i32:i32 = pid_in.try_into().unwrap();
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
    let mut screen_width: u32 = 0;
    let mut screen_height: u32 = 0;

    let ss_opt = SCREEN_SIZE.lock().unwrap();
    if ss_opt.is_some() {
        screen_width = ss_opt.unwrap().x.try_into().unwrap();
        screen_height = ss_opt.unwrap().y.try_into().unwrap();
    }

    // Defaults
    if screen_width == 0 { screen_width = 80; }
    if screen_height == 0 { screen_height = 24; }
    return (screen_width, screen_height);
}

fn average(numbers: &[f64]) -> f64 {
    let sum:f64  = numbers.iter().sum();
    let count = numbers.len() as f64;
    return sum / count;
}

// The bitrates come here in Mbits/sec.
// If the average bitrate is greater than 1000 then we divide all bitrates
// by 1000 and change the units to Gbit, for example.
// units: in/out
// return: Updated bitrates
fn scale(units: &mut String, bitrates_in: &Vec::<f64>) -> Vec::<f64> {
    let step = 1000.0;
    let step_squared = step * step;
    let step_cubed = step_squared * step;
    let step_reciprocal = 1.0 / step;
    let step_reciprocal_squared = step_reciprocal * step_reciprocal;
    let step_reciprocal_cubed = step_reciprocal_squared * step_reciprocal;

    let mut bitrates_scaled = bitrates_in.clone();
    let average = average(&bitrates_scaled);
    let mut multiply_by = 1.0;
    if average > step_cubed {
        multiply_by = step_reciprocal_cubed;
        *units = "Pbits".to_string();
    }
    else if average > step_squared {
        multiply_by = step_reciprocal_squared;
        *units = "Tbits".to_string();
    }
    else if average > step {
        multiply_by = step_reciprocal;
        *units = "Gbits".to_string();
    }
    else if average < step_reciprocal_squared {
        multiply_by = step_squared;
        *units = "bits".to_string();
    }
    else if average < step_reciprocal {
        multiply_by = step;
        *units = "Kbits".to_string();
    }

    if multiply_by != 1.0 {
        for item in &mut bitrates_scaled {
            *item *= multiply_by;
        }
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
    let mut original_chars = original.chars();
    let n = replacement.chars().count();
    let after = original_chars.by_ref().skip(n).collect::<String>();
    format!("{}{}", replacement, after)
}

#[allow(dead_code)]
fn save_to_file(filename: &str, content: &str) {
    let mut file = OpenOptions::new().append(true).create(true).open(filename).unwrap();
    write!(&mut file, "{}", content).expect("Could not write to file");
}

fn background_graph(content_graph: TextContent, args: Args) {
    let mut cmd = Command::new("iperf3");
    cmd.arg("--forceflush") // Don't buffer between lines
       .arg("--interval").arg("1") // Every second
       .arg("--time").arg("0") // Run forever
       .arg("--format").arg("m");   // In megabits

    // User-supplied options
    if args.ipv6 { cmd.arg("-6"); }
    if args.ports.is_some() { cmd.arg("-p").arg(args.ports.unwrap()); }
    if args.reverse { cmd.arg("-R"); }
    if args.udp { cmd.arg("-u"); }

    cmd.arg("--client").arg(args.server.clone())
       .stdout(Stdio::piped())
       .stderr(Stdio::piped());

    let result = cmd.spawn();
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
        let stderr_msg = format!("Checking connection to {}...", args.server.clone());
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

    let re_main = Regex::new("\\[([^\\]]+)\\]\\s(.*)$").unwrap();
    let re_bitrate = Regex::new("([\\d\\.]+)\\s(\\w+)/sec").unwrap();

    let mut bitrates = Vec::<f64>::new();
    
    let mut byte_line = Vec::new();
    for byte_result in stdout.bytes() {
        let byte = byte_result.unwrap();
        if byte == b'\n' {
            let line = String::from_utf8_lossy(&byte_line).to_string();
            byte_line.clear();

            let mut remainder = "";
            let mut bitrate: String = "".to_string();
            let mut units: String = "".to_string();
            let caps_main = re_main.captures(&line);
            if caps_main.is_some() {
                let c = caps_main.unwrap();
                remainder = &c.get(2).unwrap().as_str().trim();
            }

            if line.contains("- - -") {
                // End
            }
            else if remainder.contains("Interval") {
                // Start or end
            }
            else if !remainder.is_empty() {
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
                let graph_width = screen_width - 10;
                let graph_height = screen_height - 8;
                if bitrates.len() > graph_width as usize {
                    bitrates.remove(0);
                }

                // Scale
                let bitrates_scaled = scale(&mut units, &bitrates);

                //
                // Plot
                //

                {
                    let config = Config::default().with_width(graph_width).with_height(graph_height);
                    let content1 = plot(bitrates_scaled.clone(), config);
                    let units_pad = left_pad(units, 6);
                    let content2 = replace_at_start(&content1, &units_pad);
                    content_graph.set_content(&content2);
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

#[derive(Parser, Debug)]
struct Args {   // Alphabetical order by short
    #[arg(short = '6')]
    ipv6: bool,

    #[arg(short)]
    ports: Option<String>,

    #[arg(short = 'R')]
    reverse: bool,

    #[arg(short)]
    udp: bool,

    server: String // Mandatory
}

impl Args {
    fn friendly(&self) -> String {
        let mut out:String = self.server.clone();

        if self.ipv6 { out += " IPv6"; }
        if self.ports.is_some() {out += &(" ports ".to_owned() + &self.ports.clone().unwrap().clone()); }
        if self.reverse { out += " reverse" }
        if self.udp { out += " udp" }

        return out;
    }
}

fn main() {
    if !has_iperf3() {
        eprintln!("Please install `iperf3`");
        process::exit(1);
    }

    let args = Args::parse();

    let mut siv = cursive::default();
    let content_graph = TextContent::new("Starting...");
    let tv3 = TextView::new_with_content(content_graph.clone())
       .no_wrap()
       .with_name("tv3");

    let box3 = ResizedView::with_full_screen(tv3);
    let pan3 = Panel::new(box3).title(args.friendly());

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

    std::thread::spawn(move || { background_graph(content_graph, args) });

    siv.set_fps(1);

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
