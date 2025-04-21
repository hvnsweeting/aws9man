use aws_config::meta::region::RegionProviderChain;
use aws_config::BehaviorVersion;
use aws_sdk_health::types::EntityFilter;
use aws_sdk_health::{Client, Error};
use aws_types::region::Region;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use clap::Parser;
use csv::Writer;
use std::fs::File;
use std::path::Path;
use tokio::main;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Start date in UTC (YYYY-MM-DD format)
    #[arg(long)]
    from_utc: Option<String>,

    /// End date in UTC (YYYY-MM-DD format)
    #[arg(long)]
    to_utc: Option<String>,

    /// AWS Region
    #[arg(long)]
    region: Option<String>,
}

#[derive(Debug)]
struct HealthEvent {
    timestamp: String,
    arn: String,
    detail: String,
    affected_entities: Vec<String>,
}

#[main]
async fn main() -> Result<(), Error> {
    let args = Args::parse();

    // Calculate default dates (10 days ago to now)
    let end_time = Utc::now();
    let start_time = end_time - chrono::Duration::days(10);

    // Parse command-line dates if provided
    let start_date = match args.from_utc {
        Some(date_str) => parse_date_string(&date_str, start_time)?,
        None => start_time,
    };

    let end_date = match args.to_utc {
        Some(date_str) => parse_date_string(&date_str, end_time)?,
        None => end_time,
    };

    // Set up AWS region
    let target_region = match args.region {
        Some(region) => Region::new(region),
        None => RegionProviderChain::default_provider()
            .region()
            .await
            .expect("no default region was set"),
    };

    // Health API only available in us-east-1
    let health_api_region = Region::from_static("us-east-1");

    // Create AWS config and client
    let config = aws_config::defaults(BehaviorVersion::latest())
        .region(health_api_region)
        .load()
        .await;
    let client = Client::new(&config);

    println!(
        "Fetching AWS Health events from {} to {} for region {}",
        start_date.format("%Y-%m-%d %H:%M:%S UTC"),
        end_date.format("%Y-%m-%d %H:%M:%S UTC"),
        target_region
    );

    // Create CSV filename based on current date
    let filename = format!("{}_aws_health.csv", Utc::now().format("%Y%m%d"));
    let file_path = Path::new(&filename);

    // Create CSV writer
    let file = File::create(file_path).unwrap();
    let mut writer = Writer::from_writer(file);

    // Write CSV header
    writer
        .write_record(["Timestamp", "ARN", "Detail", "Affected Entities"])
        .unwrap();

    // Get health events
    let events = get_health_events(&client, start_date, end_date, target_region).await?;

    for event in events {
        // Print to stdout
        println!("=====");
        println!("Timestamp: {}", event.timestamp);
        println!("ARN: {}", event.arn);
        println!("Detail: {}", event.detail);
        println!("Affected Entities:");
        for entity in &event.affected_entities {
            println!("- {}", entity);
        }
        println!();

        // Write to CSV
        writer
            .write_record([
                &event.timestamp,
                &event.arn,
                &event.detail,
                &event.affected_entities.join(", "),
            ])
            .unwrap();
    }

    writer.flush().unwrap();
    println!("Events written to {}", filename);

    Ok(())
}

fn parse_date_string(date_str: &str, default: DateTime<Utc>) -> Result<DateTime<Utc>, Error> {
    match NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
        Ok(date) => Ok(Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())),
        Err(_) => {
            eprintln!(
                "Warning: Could not parse date '{}'. Using default.",
                date_str
            );
            Ok(default)
        }
    }
}

async fn get_health_events(
    client: &Client,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    target_region: Region,
) -> Result<Vec<HealthEvent>, Error> {
    let mut events = Vec::new();

    // Describe events
    let describe_events_resp = client
        .describe_events()
        .filter(
            aws_sdk_health::types::EventFilter::builder()
                .regions(target_region.to_string())
                .start_times(
                    aws_sdk_health::types::DateTimeRange::builder()
                        .from(aws_smithy_types::DateTime::from_millis(
                            start_time.timestamp_millis(),
                        ))
                        .to(aws_smithy_types::DateTime::from_millis(
                            end_time.timestamp_millis(),
                        ))
                        .build(),
                )
                .build(),
        )
        .send()
        .await?;

    let event_details = describe_events_resp.events();
    for event in event_details {
        let arn = event.arn().unwrap_or("N/A").to_string();

        // Get event details
        let event_details_resp = client
            .describe_event_details()
            .event_arns(arn.clone())
            .send()
            .await?;

        // Get affected entities
        let affected_entities_resp = client
            .describe_affected_entities()
            .set_filter(Some(
                EntityFilter::builder()
                    .event_arns(arn.clone())
                    .build()
                    .unwrap(),
            ))
            .send()
            .await?;

        let mut entity_list = Vec::new();
        let entities = affected_entities_resp.entities();
        for entity in entities {
            if let Some(entity_value) = entity.entity_value() {
                entity_list.push(entity_value.to_string());
            }
        }

        let details = event_details_resp.successful_set();
        let detail = if !details.is_empty() && details[0].event_description().is_some() {
            let desc = details[0].event_description();
            if let Some(latest) = desc.unwrap().latest_description() {
                latest.to_string()
            } else {
                "No description available".to_string()
            }
        } else {
            "No description available".to_string()
        };
        let timestamp = if let Some(start_time) = event.start_time() {
            start_time
                .fmt(aws_sdk_health::primitives::DateTimeFormat::DateTime)
                .unwrap()
        } else {
            "Unknown time".to_string()
        };

        events.push(HealthEvent {
            timestamp,
            arn,
            detail,
            affected_entities: entity_list,
        });
    }

    Ok(events)
}
