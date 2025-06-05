use chrono::{NaiveDate, NaiveDateTime, Utc};
use chrono_tz::US::Pacific;
use dialoguer::Input;
use http::header::{
    HeaderName, ACCEPT, ACCEPT_LANGUAGE, CACHE_CONTROL, CONTENT_TYPE, COOKIE, PRAGMA, REFERER,
    USER_AGENT,
};
use http::{HeaderMap, HeaderValue};
use serde::export::Formatter;
use serde::{Deserialize, Serialize};
use std::cmp::min;
use std::collections::{BTreeMap, HashSet};
use std::env;
use std::error::Error;
use std::fmt;

struct YoseClient {
    common_headers: HeaderMap,
    client: reqwest::Client,
}

impl YoseClient {
    fn new(cookies: &str) -> YoseClient {
        YoseClient {
            common_headers: common_headers(cookies),
            client: reqwest::Client::new(),
        }
    }

    fn get(self: &Self) -> reqwest::RequestBuilder {
        self.client
            .get("https://yosemite.org/wp-content/plugins/wildtrails/query.php")
            .headers(self.common_headers.clone())
    }

    async fn fetch_trailheads(&self) -> Result<Trailheads, Box<dyn Error>> {
        let trailheads = self
            .get()
            .query(&[("resource", "trailheads")])
            .send()
            .await?
            .json::<Response<Trailheads>>()
            .await?;

        if trailheads.status.r#type != "message" {
            return Err(YosemiteError::UnexpectedResponse(trailheads.status).into());
        }

        Ok(trailheads.response)
    }

    async fn fetch_report(&self, region: &str) -> Result<Vec<ReportDate>, Box<dyn Error>> {
        let report = self
            .get()
            .query(&[("resource", "report"), ("region", region)])
            .send()
            .await?
            .json::<Response<Report>>()
            .await?;

        if report.status.r#type != "message" {
            return Err(YosemiteError::UnexpectedResponse(report.status).into());
        }

        let parsed = report
            .response
            .values
            .into_iter()
            .filter_map(|dict| convert_report_values(dict))
            .collect();

        Ok(parsed)
    }
}

fn convert_report_values(mut dict: BTreeMap<String, ReportValue>) -> Option<ReportDate> {
    let date = match dict.remove("date")? {
        ReportValue::Date(date) => date,
        ReportValue::Int(_) => panic!("foo"),
    };

    let values = dict
        .into_iter()
        .filter_map(|(id, value)| match value {
            ReportValue::Int(occupancy) => Some((id, occupancy)),
            _ => None,
        })
        .collect();

    Some(ReportDate { date, values })
}

#[derive(Debug)]
enum YosemiteError {
    UnexpectedResponse(Status),
}

impl fmt::Display for YosemiteError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Error for YosemiteError {}

fn common_headers(cookies: &str) -> HeaderMap {
    let mut header_map =
        vec![
            (ACCEPT, "*/*"),
            (ACCEPT_LANGUAGE, "en-US,en;q=0.9"),
            (CACHE_CONTROL, "no-cache"),
            (CONTENT_TYPE, "application/json"),
            (HeaderName::from_static("authority"), "yosemite.org"),
            (HeaderName::from_static("sec-ch-ua"), r#""Chromium";v="88", "Google Chrome";v="88", ";Not A Brand";v="99""#),
            (HeaderName::from_static("sec-ch-ua-mobile"), "?0"),
            (HeaderName::from_static("sec-fetch-dest"), "empty"),
            (HeaderName::from_static("sec-fetch-mode"), "cors"),
            (HeaderName::from_static("sec-fetch-site"), "same-origin"),
            (HeaderName::from_static("x-requested-with"), "XMLHttpRequest"),
            (PRAGMA, "no-cache"),
            (REFERER, "https://yosemite.org/planning-your-wilderness-permit/"),
            (USER_AGENT, "Mozilla/5.0 (Macintosh; Intel Mac OS X 11_1_0) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/88.0.4324.50 Safari/537.36"),
        ]
        .into_iter()
        .map(|(k, v)| (k, HeaderValue::from_static(v)))
        .collect::<HeaderMap>();

    header_map.insert(COOKIE, HeaderValue::from_str(cookies).unwrap());

    header_map
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cookies =
        env::var("COOKIE").or_else(|_| Input::new().with_prompt("Cookie plz").interact())?;

    let client = YoseClient::new(cookies.as_str());

    let trailheads = client.fetch_trailheads().await?.values;

    let regions = trailheads
        .values()
        .filter_map(|trailhead| trailhead.region.clone())
        .collect::<HashSet<String>>();

    let reports = futures::future::join_all(
        regions
            .iter()
            .map(|region| client.fetch_report(region.as_str())),
    )
    .await;

    let mut result = BTreeMap::<NaiveDate, BTreeMap<String, u8>>::new();

    let now = Utc::now().with_timezone(&Pacific).date().naive_local();

    reports
        .into_iter()
        .filter_map(|result| result.ok())
        .flatten()
        .flat_map(|report| {
            let date = report.date;
            report
                .values
                .into_iter()
                .map(move |(id, occupancy)| (date.clone(), id, occupancy))
        })
        .filter_map(|(date, id, occupancy)| {
            // there are some unlisted trailheads... no name or capacity, we can ignore them
            let trailhead = trailheads.get(id.as_str())?;

            // adjust capacity based on the 15 day walk up period in 2020
            let capacity = if date.signed_duration_since(now).num_days() > 15 {
                trailhead.quota
            } else {
                trailhead.capacity
            };

            // sometimes they are overbooked, restrict the range
            let availability = capacity - min(capacity, occupancy);

            // discard full trailheads
            if availability > 0 {
                Some((date, trailhead.name.clone(), availability))
            } else {
                None
            }
        })
        .for_each(|(date, trailhead, availability)| {
            result
                .entry(date)
                .or_insert(BTreeMap::new())
                .insert(trailhead, availability);
        });

    for (date, values) in result {
        for (th, a) in values {
            println!("{},{},{}", date, th, a);
        }
    }

    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct Status {
    r#type: String,
    value: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Response<T> {
    status: Status,
    response: T,
}

#[derive(Debug, Serialize, Deserialize)]
struct Trailhead {
    id: String,
    name: String,
    region: Option<String>,
    quota: u8,
    capacity: u8,
    alert: Option<String>,
    notes: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Trailheads {
    timestamp: NaiveDateTime,
    values: BTreeMap<String, Trailhead>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum ReportValue {
    Date(NaiveDate),
    Int(u8),
}

#[derive(Debug, Serialize, Deserialize)]
struct Report {
    id: String,
    values: Vec<BTreeMap<String, ReportValue>>,
}

#[derive(Debug)]
struct ReportDate {
    date: NaiveDate,
    values: BTreeMap<String, u8>,
}

#[cfg(test)]
mod tests {
    use crate::{Report, Response, Trailheads};

    #[test]
    fn parse_trailheads_1() {
        let test = include_str!("trailheads_1.json");
        let res = serde_json::from_str::<Response<Trailheads>>(test);
        let resp = res.expect("derp");
        println!("{:?}", resp)
    }

    #[test]
    fn parse_trailheads_2() {
        let test = include_str!("trailheads_2.json");
        let res = serde_json::from_str::<Response<Trailheads>>(test);
        let resp = res.expect("derp");
        println!("{:?}", resp)
    }

    #[test]
    fn parse_report_1() {
        let test = include_str!("report_1.json");
        let res = serde_json::from_str::<Response<Report>>(test);
        let resp = res.expect("derp");
        println!("{:?}", resp)
    }

    #[test]
    fn parse_report_2() {
        let test = include_str!("report_2.json");
        let res = serde_json::from_str::<Response<Report>>(test);
        let resp = res.expect("derp");
        println!("{:?}", resp)
    }
}
