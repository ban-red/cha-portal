//! S9 bench: `s9-usbip-loopback --backend usbip|uhid [--rate HZ] [--seconds S] [--hold S]`

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("Linux only");
}

#[cfg(target_os = "linux")]
fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "linux")]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;

    use s9_usbip_loopback::linux::{self, Backend, Loopback, Uhid};

    let mut backend = "usbip".to_string();
    let (mut rate, mut seconds, mut hold) = (1000.0f64, 10.0f64, 0.0f64);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--backend" => backend = value()?,
            "--rate" => rate = value()?.parse()?,
            "--seconds" => seconds = value()?.parse()?,
            "--hold" => hold = value()?.parse()?,
            other => {
                return Err(format!(
                    "unknown argument {other} (--backend usbip|uhid --rate --seconds --hold)"
                )
                .into());
            }
        }
    }

    let before = linux::hidraw_names();
    let mut dev: Box<dyn Backend> = match backend.as_str() {
        "usbip" => Box::new(Loopback::attach()?),
        "uhid" => Box::new(Uhid::create()?),
        other => return Err(format!("unknown backend {other}").into()),
    };
    if backend == "usbip" {
        // `Loopback::attach` returned, so the port is assigned; wait for USED.
        eprintln!("attached, waiting for enumeration");
    }
    let node = linux::find_hidraw(&before, dev.via(), Duration::from_secs(8))
        .ok_or("no hidraw node appeared (is a driver bound? see dmesg)")?;
    let ev: Vec<String> = linux::evidence(&node)
        .iter()
        .map(|(k, v)| format!("\"{k}\":\"{v}\""))
        .collect();
    eprintln!("enumerated: {}", ev.join(" "));
    if hold > 0.0 {
        eprintln!("holding for {hold} s");
        std::thread::sleep(Duration::from_secs_f64(hold));
    }
    let r = linux::run_bench(dev.as_mut(), &node, rate, seconds)?;
    println!(
        "{{\"backend\":\"{backend}\",\"rate_hz\":{rate},\"seconds\":{seconds},\"sent\":{},\"received\":{},\"lost\":{},\"out_of_order\":{},\"delivered_hz\":{:.1},\"latency_us\":{{\"min\":{:.1},\"p50\":{:.1},\"p95\":{:.1},\"p99\":{:.1},\"max\":{:.1}}},\"producer_late_us\":{{\"p99\":{:.1},\"max\":{:.1}}},\"evidence\":{{{}}}}}",
        r.sent,
        r.received,
        r.lost,
        r.out_of_order,
        r.delivered_hz,
        r.lat_us[0],
        r.lat_us[1],
        r.lat_us[2],
        r.lat_us[3],
        r.lat_us[4],
        r.late_us[0],
        r.late_us[1],
        ev.join(",")
    );
    drop(dev); // detaches / destroys
    Ok(())
}
