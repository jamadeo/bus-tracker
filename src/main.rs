use clap::Parser;
use reqwest::{header, Client as HttpClient};
use rumqttc::{AsyncClient, MqttOptions, QoS};
use serde::Deserialize;
use serde_json;
use serde_json::json;
use std::{
    fs,
    time::{Duration, Instant},
};

const BASE_URL: &str = "https://api.glympse.com/v2";

#[derive(Parser, Debug, Clone, Deserialize)]
struct Opts {
    // --- Glympse Configuration ---
    glympse_api_key: String,
    glympse_username: String,
    glympse_password: String,
    glympse_group_id: String,

    // --- Device & MQTT Configuration ---
    device_id: String,
    device_name: String,
    mqtt_topic_prefix: String,
    runtime_seconds: u64,
    poll_interval_s: u64,
}

impl Opts {
    pub fn from_config_path(config_path: &str) -> Self {
        // Read the file. If it doesn't exist, we're likely not in a HA Add-on environment.
        let data = fs::read_to_string(config_path)
            .inspect_err(|e| eprintln!("error reading: {}", e))
            .expect("could not read options");

        // Parse the JSON into the Args struct
        serde_json::from_str(&data)
            .inspect_err(|e| eprintln!("error parsing: {}", e))
            .expect("could not parse options")
    }
}

#[derive(Parser, Debug, Clone)]
#[command(author, version, about)]
struct Args {
    #[arg(long)]
    config_options_path: String,

    #[arg(long, default_value_t = false)]
    stdout_only: bool,
}

#[derive(Deserialize, Debug)]
struct SupervisorServiceResponse {
    result: String,
    data: SupervisorMqttData,
}

#[derive(Deserialize, Debug)]
struct SupervisorMqttData {
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<String>,
}

#[derive(Deserialize, Debug)]
struct LoginResponse {
    result: String,
    response: LoginData,
}
#[derive(Deserialize, Debug)]
struct LoginData {
    access_token: String,
}
#[derive(Deserialize, Debug)]
struct LocationResponse {
    result: String,
    response: LocationData,
}
#[derive(Deserialize, Debug)]
struct LocationData {
    location: Vec<Vec<f64>>,
}
#[derive(Debug)]
struct LocationPoint {
    #[allow(dead_code)]
    timestamp: i64,
    lat: f64,
    lng: f64,
    altitude: i64,
    heading: i64,
    speed: i64,
    accuracy: i64,
    #[allow(dead_code)]
    source: i64,
}

#[derive(Deserialize, Debug)]
struct GroupResponse {
    result: String,
    response: GroupData,
}
#[derive(Deserialize, Debug)]
struct GroupData {
    members: Vec<GroupMember>,
}
#[derive(Deserialize, Debug)]
struct GroupMember {
    invite: String,
}

async fn get_invite_from_group(
    client: &HttpClient,
    token: &str,
    group_id: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let url = format!("{}/groups/{}", BASE_URL, group_id);
    let res = client
        .get(&url)
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(format!("Group API failed with status {}", res.status()).into());
    }
    let data: GroupResponse = res.json().await?;
    if data.result != "ok" {
        return Err(format!("Group API failed: {}", data.result).into());
    }
    let invite = data
        .response
        .members
        .first()
        .ok_or("No members found in group")?
        .invite
        .clone();
    Ok(invite)
}

async fn fetch_mqtt_credentials(
    client: &HttpClient,
) -> Result<SupervisorMqttData, Box<dyn std::error::Error>> {
    let token = std::env::var("SUPERVISOR_TOKEN")
        .map_err(|_| "SUPERVISOR_TOKEN environment variable is missing")?;

    let res = client
        .get("http://supervisor/services/mqtt")
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .send()
        .await?;

    if !res.status().is_success() {
        return Err(format!(
            "Failed to fetch MQTT credentials from Supervisor: {}",
            res.status()
        )
        .into());
    }

    let parsed: SupervisorServiceResponse = res.json().await?;

    if parsed.result != "ok" {
        return Err("Supervisor returned non-ok result for MQTT credentials".into());
    }

    Ok(parsed.data)
}

fn build_client() -> reqwest::Result<HttpClient> {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::ACCEPT,
        "application/json, text/plain, */*".parse().unwrap(),
    );
    headers.insert(header::ACCEPT_LANGUAGE, "en-US,en;q=0.9".parse().unwrap());
    headers.insert(header::CONNECTION, "keep-alive".parse().unwrap());
    headers.insert(header::ORIGIN, "https://glympse.com".parse().unwrap());
    headers.insert(header::REFERER, "https://glympse.com/".parse().unwrap());
    headers.insert(header::USER_AGENT, "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36".parse().unwrap());
    HttpClient::builder().default_headers(headers).build()
}

fn decode_location_stream(raw: &[Vec<f64>]) -> Vec<LocationPoint> {
    let mut points = Vec::with_capacity(raw.len());
    let (mut ts, mut lat, mut lng, mut alt, mut heading, mut speed, mut accuracy, mut source) =
        (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for row in raw {
        if row.len() < 8 {
            continue;
        }
        ts += row[0];
        lat += row[1];
        lng += row[2];
        alt += row[3];
        heading += row[4];
        speed += row[5];
        accuracy += row[6];
        source += row[7];
        points.push(LocationPoint {
            timestamp: ts.round() as i64,
            lat: lat as f64 / 1_000_000.0,
            lng: lng as f64 / 1_000_000.0,
            altitude: alt.round() as i64,
            heading: heading.round() as i64,
            speed: speed.round() as i64,
            accuracy: accuracy.round() as i64,
            source: source.round() as i64,
        });
    }
    points
}

async fn login(client: &HttpClient, args: &Opts) -> Result<String, Box<dyn std::error::Error>> {
    let url = format!(
        "{}/account/login?password={}&api_key={}&username={}",
        BASE_URL, args.glympse_password, args.glympse_api_key, args.glympse_username
    );
    let res = client.get(&url).send().await?;
    if !res.status().is_success() {
        return Err(format!("Login failed with status {}", res.status()).into());
    }
    let data: LoginResponse = res.json().await?;
    if data.result != "ok" {
        return Err(format!("Login failed: {}", data.result).into());
    }
    Ok(data.response.access_token)
}

async fn get_location(
    client: &HttpClient,
    token: &str,
    invite_code: &str,
) -> Result<LocationResponse, Box<dyn std::error::Error>> {
    let url = format!("{}/invites/{}?next=0", BASE_URL, invite_code);
    let res = client
        .get(&url)
        .header(header::AUTHORIZATION, format!("Bearer {}", token))
        .header(
            "X-GlympseAgent",
            "app=consumer-app&ver=0.0.1&comp=GlympseViewerWeb&comp_ver=7.0.3",
        )
        .send()
        .await?;
    if !res.status().is_success() {
        return Err(format!("Location API failed with status {}", res.status()).into());
    }
    let data: LocationResponse = res.json().await?;
    if data.result != "ok" {
        return Err(format!("Location API failed: {}", data.result).into());
    }
    Ok(data)
}

async fn setup_mqtt(
    mqtt_data: &SupervisorMqttData,
    args: &Opts,
) -> Result<AsyncClient, Box<dyn std::error::Error>> {
    // Generate a unique MQTT client ID based on the configured device ID
    let client_id = format!("{}_client", args.device_id);
    let mut mqttoptions = MqttOptions::new(client_id, &mqtt_data.host, mqtt_data.port);

    if let (Some(user), Some(pass)) = (&mqtt_data.username, &mqtt_data.password) {
        println!(
            "Successfully fetched Supervisor credentials for user: {}",
            user
        );
        mqttoptions.set_credentials(user, pass);
    }

    mqttoptions.set_keep_alive(Duration::from_secs(5));
    let (client, mut eventloop) = AsyncClient::new(mqttoptions, 10);

    tokio::spawn(async move {
        loop {
            if let Err(e) = eventloop.poll().await {
                eprintln!("MQTT connection warning: {:?}", e);
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    });

    let config_topic = format!("homeassistant/device_tracker/{}/config", args.device_id);
    let state_topic = format!("{}/state", args.mqtt_topic_prefix);
    let attr_topic = format!("{}/attributes", args.mqtt_topic_prefix);

    let config_payload = json!({
        "name": args.device_name,
        "state_topic": state_topic,
        "json_attributes_topic": attr_topic,
        "unique_id": args.device_id,
        "source_type": "gps",
        "icon": "mdi:bus",
        "device": {
            "identifiers": [args.device_id],
            "name": args.device_name,
            "manufacturer": "Glympse"
        }
    });

    client
        .publish(
            config_topic,
            QoS::AtLeastOnce,
            true,
            config_payload.to_string(),
        )
        .await?;

    Ok(client)
}

async fn publish_location_mqtt(
    mqtt_client: &AsyncClient,
    point: &LocationPoint,
    args: &Opts,
) -> Result<(), Box<dyn std::error::Error>> {
    let state_topic = format!("{}/state", args.mqtt_topic_prefix);
    let attr_topic = format!("{}/attributes", args.mqtt_topic_prefix);

    mqtt_client
        .publish(&state_topic, QoS::AtLeastOnce, false, "not_home")
        .await?;

    let attr_payload = json!({
        "latitude": point.lat,
        "longitude": point.lng,
        "gps_accuracy": point.accuracy,
        "altitude": point.altitude,
        "heading": point.heading,
        "speed": point.speed,
    });

    mqtt_client
        .publish(
            &attr_topic,
            QoS::AtLeastOnce,
            false,
            attr_payload.to_string(),
        )
        .await?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let opts = Opts::from_config_path(&args.config_options_path);
    let http_client = build_client()?;

    let mqtt_client = if !args.stdout_only {
        let mqtt_credentials = fetch_mqtt_credentials(&http_client).await?;
        println!(
            "Connecting to MQTT broker at {}:{}...",
            mqtt_credentials.host, mqtt_credentials.port
        );
        Some(setup_mqtt(&mqtt_credentials, &opts).await?)
    } else {
        None
    };

    let runtime = Duration::from_secs(opts.runtime_seconds);
    let start_time = Instant::now();

    eprintln!(
        "Starting session for '{}' — polling every {}s, will run for {}s",
        opts.device_name, opts.poll_interval_s, opts.runtime_seconds
    );

    let token = login(&http_client, &opts).await?;
    let invite_code =
        get_invite_from_group(&http_client, &token, &opts.glympse_group_id).await?;
    eprintln!(
        "Resolved group '{}' to invite code '{}'",
        opts.glympse_group_id, invite_code
    );
    let mut poll_count = 0;

    while start_time.elapsed() < runtime {
        poll_count += 1;

        match get_location(&http_client, &token, &invite_code).await {
            Ok(data) => {
                let points = decode_location_stream(&data.response.location);
                if let Some(latest) = points.last() {
                    if let Some(client) = &mqtt_client {
                        if let Err(e) = publish_location_mqtt(client, latest, &opts).await {
                            eprintln!("[poll #{}] MQTT Publish Error: {}", poll_count, e);
                        }
                    } else {
                        println!(
                            "[poll #{}] lat={:.6}  lng={:.6}  speed={} ({} pts)",
                            poll_count,
                            latest.lat,
                            latest.lng,
                            latest.speed,
                            points.len()
                        );
                    }
                } else {
                    println!("[poll #{}] No location data", poll_count);
                }
            }
            Err(e) => eprintln!("[poll #{}] Error: {}", poll_count, e),
        }

        if start_time.elapsed() < runtime {
            tokio::time::sleep(Duration::from_secs(opts.poll_interval_s)).await;
        }
    }

    println!("\nSession ended after {} polls.", poll_count);
    Ok(())
}
