use std::io::Read;
use std::process;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::error::Error;
use std::io::Write;
use std::time::Duration;
use nix::unistd::Pid;
use lazy_static::lazy_static;
use clap::Parser;
use nix::sys::signal::{kill,Signal};
use timeout_readwrite::TimeoutReader;
use regex::Regex;
use cursive::{Cursive,XY};
use cursive::reexports::crossbeam_channel::Sender;
use cursive::event::{Event,Key};
use cursive::align::HAlign;
use cursive::views::{ResizedView, Dialog, LinearLayout, TextContent, TextView, Panel, EditView, NamedView, SelectView};
use cursive::traits::*;
use cursive::menu::Tree;
use rasciigraph::{plot, Config};

#[derive(Clone, PartialEq, Eq)]
enum State {
    Normal,
    ReloadRequested,
    Quit
}

//
// Globals
//

lazy_static! {
    static ref IPERF3_PID: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
    static ref SCREEN_SIZE: Arc<Mutex<Option<XY<usize>>>> = Arc::new(Mutex::new(None));
    static ref ARGS: Arc<Mutex<Option<Args>>> = Arc::new(Mutex::new(None));
    static ref STATE: Arc<Mutex<Option<State>>> = Arc::new(Mutex::new(None));
}

fn has_iperf3() -> bool {
   return Command::new("which").arg("iperf3").stdout(Stdio::null()).stderr(Stdio::null()).status().expect("Could not run 'which iperf3'").success();
}

fn save_pid(pid_in: u32) {
    let mut pid_opt = IPERF3_PID.lock().unwrap();
    let result: Result<i32, _> = pid_in.try_into();
    if result.is_err() { return; }
    let pid_i32 = result.unwrap();
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

fn save_server(server: String) {
    let mut args_opt = ARGS.lock().unwrap();
    if args_opt.is_none() { return; }
    let args_ref = args_opt.as_mut().unwrap();
    args_ref.clear();
    args_ref.server_in_cmd = Option::Some(server);
}

fn save_args(args_in: &Args) {
    let mut args_opt = ARGS.lock().unwrap();
    *args_opt = Some(args_in.clone());
}

fn get_args() -> Args {
    let args_opt = ARGS.lock().unwrap();
    if args_opt.is_none() { return Args::parse(); }
    let args = args_opt.as_ref().unwrap().clone();
    return args;
}

fn get_state() -> State{
    let state_opt = STATE.lock().unwrap();
    if state_opt.is_none() { return State::Normal; }
    let state = state_opt.clone().unwrap().clone();
    return state;
}

fn save_state(state_in: State) {
    let mut state_opt = STATE.lock().unwrap();
    *state_opt = Some(state_in.clone());
}

//
// Utilites
//

fn mkerr(txt: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, txt)
}

fn save_file_contents(filename: &str, content: &str) -> std::io::Result<()> {
    let mut file = std::fs::File::create(filename)?;
    file.write_all(content.as_bytes())?;
    Ok(())
}

fn log(txt: &str) {
    // Don't use /var/log because not all users are allowed
    let filename = "/tmp/iperf3-tui.log";
    let max_size = 1024 * 1024;

    let meta_result = std::fs::metadata(filename);
    if meta_result.is_ok() {
        let len = meta_result.unwrap().len();
        if len > max_size {
            let old = filename.to_owned() + ".old";
            let _ = std::fs::rename(filename, old);
        }
    }

    let mut file = std::fs::OpenOptions::new().write(true).append(true).open(filename).unwrap();
    let _ = writeln!(file, "{}", txt);
}

//
// Graphing
//

fn average(numbers: &[f64]) -> f64 {
    let sum:f64  = numbers.iter().sum();
    let count = numbers.len() as f64;
    return sum / count;
}

// The bitrates come here in Mbits/sec.
// If the average bitrate is greater than 1000 then we divide all bitrates
// by 1000 and change the units to Gbit, for example.
// units: is in/out
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

// Needs to be UTF-8 safe
fn replace_at_start(original: &str, replacement: &str) -> String {
    let mut original_chars = original.chars();
    let n = replacement.chars().count();
    let after = original_chars.by_ref().skip(n).collect::<String>();
    return format!("{}{}", replacement, after)
}

fn background_graph_worker(sink: &Sender<Box<dyn FnOnce(&mut Cursive) + Send>>, content_graph: &TextContent) {
    save_state(State::Normal);
    let args = get_args();
    let friendly = args.friendly();
    sink.send(Box::new(|s: &mut Cursive| {
        s.call_on_name("pan3", |view: &mut NamedView<Panel<NamedView<ResizedView<NamedView<TextView>>>>> | view.get_mut().set_title(friendly));
    })).unwrap();

    let server_opt = args.get_server();
    if server_opt.is_none() {
        content_graph.set_content("Server is not selected.\nYou can quit, specify a server on the command line\nor select a server from the menu");
        return;
    }

    let mut cmd = Command::new("iperf3");
    cmd.arg("--forceflush") // Don't buffer between lines
       .arg("--interval").arg("1") // Every second
       .arg("--time").arg("0") // Run forever
       .arg("--format").arg("m");   // In megabits

    // User-supplied options
    if args.ipv6 { cmd.arg("-6"); }
    if args.ports.is_some() { cmd.arg("-p").arg(args.get_ports()); }
    if args.reverse { cmd.arg("-R"); }
    if args.udp { cmd.arg("-u"); }

    let server_str1 = server_opt.clone().unwrap();
    let server_str2 = server_opt.clone().unwrap();
    if server_opt.is_some() { cmd.arg("--client").arg(server_str1); }
    log(&format!("background_graph_worker: server={}", server_str2));

    cmd.stdout(Stdio::piped())
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
        let server = args.get_server_as_string();
        let stderr_msg = format!("Checking connection to {} ...", server);
        content_graph.set_content(stderr_msg);
   
        let stderr_result = child.stderr;

        if stderr_result.is_none() {
            content_graph.set_content("Could not get stderr from iperf3");
            return;
        }
        let stderr = stderr_result.unwrap();

        let mut stderr_data = String::new();
        let mut stderr_rdr = TimeoutReader::new(stderr, Duration::from_secs(5));
        let _ = stderr_rdr.read_to_string(&mut stderr_data);
        if !stderr_data.is_empty() {
            let stderr_err = format!("{}\nYou can quit or select another server", stderr_data.trim());
            content_graph.set_content(stderr_err);
            return;
        }
        else {
            let stderr_msg = format!("No immediate error from {} ...", server);
            content_graph.set_content(stderr_msg);
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
    let mut stdout_data : [u8;1] = [0;1];
    let mut stdout_rdr = TimeoutReader::new(stdout, Duration::from_secs(5));
    loop {
        if get_state() == State::ReloadRequested {
            return;
        }

        let rdr_result = stdout_rdr.read(&mut stdout_data);
        if rdr_result.is_err() {
            // eprintln!("Error reading");
            continue;
        }
        let len = rdr_result.unwrap();
        if len < 1 {
            // eprintln!("Read zero");
            continue;
        }

        let byte = stdout_data[0];

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

                let bitrate_result = bitrate.parse::<f64>();
                if bitrate_result.is_err() { continue; }
                let bitrate_f64 = bitrate_result.unwrap();
                bitrates.push(bitrate_f64.to_owned());
                let graph_width = screen_width - 10;
                let graph_height = screen_height - 8;
                while bitrates.len() > graph_width as usize {
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

fn background_graph(sink: &Sender<Box<dyn FnOnce(&mut Cursive) + Send>>, content_graph: &TextContent) {
    loop {
        if get_state() == State::Quit { return; }
        background_graph_worker(sink, content_graph);
        kill_pid();

        loop {
            if get_state() != State::Normal {
                break;
            }
        }
    }
}

fn on_quit(siv: &mut Cursive) {
    log("on_quit");
    save_state(State::Quit);
    kill_pid();
    siv.quit();
}

//
// Arguments to iperf3 (and us)
//

#[derive(Parser, Debug, Clone, Default)]
struct Args {   // Alphabetical order by short
    #[arg(short = '6')]
    ipv6: bool,

    #[arg(short)]
    ports: Option<String>,

    #[arg(short = 'R')]
    reverse: bool,

    #[arg(short)]
    udp: bool,

    #[arg(short = 'c')]
    server_in_option: Option<String>,

    // No dash in front of the server and its not required
    server_in_cmd: Option<String>
}

impl Args {
    fn clear(&mut self) {
        self.ipv6 = false;
        self.ports = None;
        self.reverse = false;
        self.udp = false;
        self.server_in_option = None;
        self.server_in_cmd = None;
    }

    fn get_ports(&self) -> String {
        if self.ports.is_none() { return "".to_string(); }
        return self.ports.clone().unwrap().clone();
    }

    fn get_server(&self) -> Option<String> {
        if self.server_in_option.is_some() { return self.server_in_option.clone(); }
        if self.server_in_cmd.is_some() { return self.server_in_cmd.clone(); }
        None
    }

    fn get_server_as_string(&self) -> String {
        let opt = self.get_server();
        if opt.is_none() { return "".to_string(); }
        return opt.unwrap();
    }

    fn friendly(&self) -> String {
        let mut out:String = String::default();

        let opt = self.get_server();
        if opt.is_some() {
            out += &opt.unwrap();
        }
        else {
            out += "(server not specified)";
        }

        if self.ipv6 { out += " IPv6"; }
        if self.ports.is_some() { out += &(" ports ".to_owned() + &self.get_ports()) }
        if self.reverse { out += " reverse" }
        if self.udp { out += " udp" }

        return out;
    }
}

//
// Servers file
//

fn get_servers_filename() -> std::io::Result<String> {
    let subfolder = "iperf3-tui";
    let basename = "unparsed_servers.csv";

    let mut config_path = dirs::config_dir().expect("Could not find config directory");
    config_path.push(subfolder);
    std::fs::create_dir_all(&config_path)?;
    let abs_filename = config_path.join(basename);
    let str = abs_filename.to_str().unwrap().to_string();
    return Ok(str);
}

#[derive(Default,Debug)]
#[allow(dead_code)]
struct UnparsedServer {
    cmd: String,
    options: String,
    speed: String,
    country: String,
    provider: String,
    continent: String,
    site: String,
    status: String,
}

#[derive(Default,Debug)]
struct ParsedServer {
    args: Args,
    speed: String,
    country: String,
    provider: String,
    continent: String,
    site: String,   // eg City
    status: String,
}

impl ParsedServer {
    fn friendly(&self) -> String {
        let mut out = format!("{} {} {} {}", self.continent, self.country, self.site, self.provider);
        if !self.speed.is_empty() {
            out += &format!(" {} GB/s", self.speed);
        }
        return out;
    }
}

fn parse_server(unparsed: &UnparsedServer) -> ParsedServer {
    let mut parsed = ParsedServer::default();
    parsed.speed = unparsed.speed.clone();
    parsed.country = unparsed.country.clone();
    parsed.provider = unparsed.provider.clone();
    parsed.continent = unparsed.continent.clone();
    parsed.site = unparsed.site.clone();
    parsed.status = unparsed.status.clone();
    let clean_options = str::replace(&unparsed.options, ",", " ");
    let line = unparsed.cmd.clone() + " " + &clean_options;
    parsed.args = Args::parse_from(line.split_whitespace());
    return parsed;
}

fn parse_servers_file(filename: &String) -> std::io::Result<Vec<ParsedServer>> {
    let mut out: Vec<ParsedServer> = Vec::new();
    let file = std::fs::File::open(filename)?;
    let reader = std::io::BufReader::new(file);
    let mut rdr = csv::Reader::from_reader(reader);
    for result in rdr.records() {
        if result.is_err() { continue; }
        let record = result.unwrap();
        if record.len() < 8 { continue; }
        let mut unparsed = UnparsedServer::default();
        unparsed.cmd = record.get(0).unwrap().to_string();
        unparsed.options = record.get(1).unwrap().to_string();
        unparsed.speed = record.get(2).unwrap().to_string();
        unparsed.country = record.get(3).unwrap().to_string();
        unparsed.site = record.get(4).unwrap().to_string();
        unparsed.provider = record.get(5).unwrap().to_string();
        unparsed.continent = record.get(6).unwrap().to_string();
        unparsed.status = record.get(7).unwrap().to_string();
        let parsed = parse_server(&unparsed);
        out.push(parsed);
    }

    if out.len() == 0 {
        return Err(mkerr("No servers found in the file (could not parse it)"));
    }

    Ok(out)
}

fn servers_filename_has_content(filename: &String) -> bool {
    let meta_result = std::fs::metadata(filename);
    if meta_result.is_err() { return false; }
    let len = meta_result.unwrap().len();
    return len > 10;
}

fn servers_file_has_content() -> bool {
    let filename_result = get_servers_filename();
    if filename_result.is_err() { return false; }
    let filename = filename_result.unwrap();

    servers_filename_has_content(&filename)
}

fn get_parsed_servers() -> std::io::Result<Vec<ParsedServer>> {
    let filename_result = get_servers_filename();
    if filename_result.is_err() {
        return Err(mkerr("Could not get filename for servers"));
    }

    let filename = filename_result.unwrap();
    if !servers_filename_has_content(&filename) {
        return Err(mkerr("Please download servers first"));
    }

    parse_servers_file(&filename)
}

fn download_url(url: &str) -> Result<String, Box<dyn Error>> {
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(20)).build()?;
    let response = client.get(url).send()?;

    if response.status().is_success() {
        let body = response.text()?;
        Ok(body)
    } else {
        Err(format!("Could not download: HTTP {}", response.status()).into())
    }
}

// Doesn't return an error - but sets in the status
fn download_servers(sink: &Sender<Box<dyn FnOnce(&mut Cursive) + Send>>) {
    let result = download_url("https://export.iperf3serverlist.net/unparsed_iperf3_servers.csv");

    let status;
    if result.is_err() {
        status = result.unwrap_err().to_string();
    }
    else {
        let body = result.unwrap();
        let filename_result = get_servers_filename();
        if filename_result.is_err() {
            status = filename_result.unwrap_err().to_string();
        }
        else {
            let filename = filename_result.unwrap();
            let save_result = save_file_contents(&filename, &body);
            if save_result.is_err() {
                status = "Downloaded list of servers but could not save to a file - permission?".to_string();
            }
            else {
                let servers_result = get_parsed_servers();
                if servers_result.is_err() {
                    status = format!("Downloaded list of servers but {}", servers_result.unwrap_err());
                }
                else {
                    let servers = servers_result.unwrap();
                    status = format!("Downloaded {} servers", servers.len());
                }
            }
        }
    }
    sink.send(Box::new(|s: &mut Cursive| {
        s.call_on_name("status", |view: &mut NamedView<TextView> | view.get_mut().set_content(status));
    })).unwrap();

}

//
//  Dialogs
//

fn download_servers_dialog(siv: &mut Cursive) {
    let sink = siv.cb_sink().clone();
    siv.add_layer(
        Dialog::new()
        .title("Download list of iperf3 servers")
        .padding_lrtb(1, 1, 1, 0)
        .content(
            TextView::new("Downloading...")
                .with_name("status")
                .fixed_width(50),
        )
        .button("Close", |s| { s.pop_layer(); })
    );

    std::thread::spawn(move || { download_servers(&sink) });
}

fn select_server_dialog(siv: &mut Cursive) {
    let servers_result = get_parsed_servers();
    if servers_result.is_err() {
        siv.add_layer(
            Dialog::new()
            .title("Select an iperf3 server")
            .padding_lrtb(1, 1, 1, 0)
            .content(TextView::new(servers_result.unwrap_err().to_string()))
            .button("Close", |s| { s.pop_layer(); }));
        return;
    }
    let servers = servers_result.unwrap();

    let mut select = SelectView::<ParsedServer>::new()
        .on_submit(|s, item| {
            save_args(&item.args);
            save_state(State::ReloadRequested);
            log(&format!("select_server_dialog: user selected {} with {}", item.friendly(), item.args.friendly()));
            s.pop_layer();
        });

    for server in servers {
        select.add_item(server.friendly(), server);
    }

    siv.add_layer(Dialog::around(select.scrollable())
        .title("Select an iperf3 server")
        .button("Cancel", |s| { s.pop_layer(); } )
    );
}

fn enter_server_dialog(siv: &mut Cursive) {
    siv.add_layer(
        Dialog::new()
        .title("Enter an iperf3 server name or IP-address")
        .padding_lrtb(1, 1, 1, 0)
        .content(
            EditView::new()
                .with_name("server")
                .fixed_width(50),
        )
        .button("OK", |s| {
            let server = s.call_on_name("server", |view: &mut EditView| view.get_content()).unwrap();

            let server_str = server.to_string();
            save_server(server_str.clone());
            save_state(State::ReloadRequested);
            log(&format!("enter_server_dialog: user entered {}", server_str).to_string());
            s.pop_layer();
        })
        .button("Cancel", |s| { s.pop_layer(); })
    );
}

fn about_dialog(siv: &mut Cursive) {
    let info = "iperf3-tui\nby Dave McKellar\nhttps://github.com/dmdmdm\n\nServer List from\nhttps://www.iperf3serverlist.net\nWith thanks!";

    siv.add_layer(
        Dialog::info(info)
        .title("About")
        .padding_lrtb(1, 1, 1, 0)
    );
}

fn add_menu(siv: &mut Cursive) {
    let download_txt = if servers_file_has_content() { "Refresh list of iperf3 servers"} else { "Download list of iperf3 servers" };
	siv.menubar()
	    .add_subtree(
	        "File",
	        Tree::new()
	            .leaf(download_txt, |s| download_servers_dialog(s))
	            .leaf("Select Server", |s| select_server_dialog(s))
	            .leaf("Enter Server", |s| enter_server_dialog(s))
	            .leaf("About", |s| about_dialog(s))
	            .leaf("Quit", on_quit)
	    );
	
    siv.set_autohide_menu(false);
    siv.add_global_callback(Key::Esc, |s| s.select_menubar());
}

fn main() {
    if !has_iperf3() {
        eprintln!("Please install `iperf3`");
        process::exit(1);
    }

    let args = Args::parse();
    save_args(&args);
    save_state(State::Normal);

    let mut siv = cursive::default();
    let sink = siv.cb_sink().clone();
    let content_graph = TextContent::new("Starting...");
    let tv3 = TextView::new_with_content(content_graph.clone())
       .no_wrap()
       .with_name("tv3");

    let box3 = ResizedView::with_full_screen(tv3).with_name("box3");
    let pan3 = Panel::new(box3).title(args.friendly()).with_name("pan3");

    siv.add_layer(
       Dialog::around(
           LinearLayout::vertical()
               .child(pan3)
       )
       .title("iperf3-tui")
       .h_align(HAlign::Center),
    );

    add_menu(&mut siv);

    siv.add_global_callback('q', on_quit);

    std::thread::spawn(move || { background_graph(&sink, &content_graph) });

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
            // We could add tests here
            return;
        }

        let padded = left_pad("hello".to_string(), 9);
        assert_eq!(padded, "    hello");
    }
}
