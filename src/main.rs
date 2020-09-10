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
            (HeaderName::from_static("sec-fetch-dest"), "empty"),
            (HeaderName::from_static("sec-fetch-mode"), "cors"),
            (HeaderName::from_static("sec-fetch-site"), "same-origin"),
            (HeaderName::from_static("x-requested-with"), "XMLHttpRequest"),
            (PRAGMA, "no-cache"),
            (REFERER, "https://yosemite.org/planning-your-wilderness-permit/"),
            (USER_AGENT, "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_6) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/85.0.4183.69 Safari/537.36"),
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
    fn parse_trailheads() {
        let test = r#"{"status":
            {"type":"message","value":"trailheads found."},
            "response":{
                "timestamp":"2020-09-06T22:43:55",
                "values":
                    {"w35":
                        {"id":"w35","name":"Alder Creek","wpsName":"Alder Creek","region":"ww","latitude":null,"longitude":null,"description":null,"quota":18,"capacity":30,"alert":"This trailhead is currently <b>closed<\/b> due to the <span><a href=\"https:\/\/inciweb.nwcg.gov\/incident\/7147\/\" target=\"_blank\">Creek Fire<\/a>.<\/span>","notes":null},
                        "b11":{"id":"b11","name":"Aspen Valley","wpsName":"Aspen Valley","region":"bf","latitude":null,"longitude":null,"description":null,"quota":6,"capacity":10,"alert":null,"notes":"<li>This trail is not used often and portions of the trail are overgrown with vegetation. Bring a good map of the area.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300' from water.<\/li>"},"h29b":{"id":"h29b","name":"Beehive Meadow","wpsName":"Beehive Meadow","region":"hh","latitude":null,"longitude":null,"description":null,"quota":21,"capacity":35,"alert":null,"notes":null},"w32":{"id":"w32","name":"Bridalveil Creek","wpsName":"Bridalveil Creek","region":"ww","latitude":null,"longitude":null,"description":null,"quota":15,"capacity":25,"alert":null,"notes":"<li>You may not camp at Bridalveil Creek Campground with this permit.<\/li>"},"x03":{"id":"x03","name":"Budd Creek (cross-country only)","wpsName":"Budd Creek (cross-country only)","region":"tm","latitude":null,"longitude":null,"description":null,"quota":3,"capacity":5,"alert":"This is a cross-country trailhead, and the trail\/route is not maintained. All members of the party must be proficient at backcountry navigation.","notes":"<li>Camping is prohibited in the Budd Lake and Elizabeth Lake drainages.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"t21":{"id":"t21","name":"Cathedral Lakes","wpsName":"Cathedral Lakes","region":"tm","latitude":null,"longitude":null,"description":null,"quota":15,"capacity":25,"alert":null,"notes":"<li>Fires are prohibited at Upper and Lower Cathedral Lakes.<\/li><li>Camping is prohibited in the Budd Lake and Elizabeth Lake drainages.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"w36":{"id":"w36","name":"Chilnualna Falls","wpsName":"Chilnualna Falls","region":"ww","latitude":null,"longitude":null,"description":null,"quota":24,"capacity":40,"alert":"This trailhead is currently <b>closed<\/b> due to the <span><a href=\"https:\/\/inciweb.nwcg.gov\/incident\/7147\/\" target=\"_blank\">Creek Fire<\/a>.<\/span>","notes":"<li>Only use existing fire rings. Building new fire rings is not allowed.<\/li><li>You must be at the top of Chilnualna Falls before camping.<\/li>"},"h26a":{"id":"h26a","name":"Cottonwood Creek","wpsName":"Cottonwood Creek","region":"hh","latitude":null,"longitude":null,"description":null,"quota":12,"capacity":20,"alert":null,"notes":"<li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"w30":{"id":"w30","name":"Deer Camp","wpsName":"Deer Camp","region":"ww","latitude":null,"longitude":null,"description":null,"quota":15,"capacity":25,"alert":"This trailhead is currently <b>closed<\/b> due to the <span><a href=\"https:\/\/inciweb.nwcg.gov\/incident\/7147\/\" target=\"_blank\">Creek Fire<\/a>.<\/span>","notes":null},"d01":{"id":"d01","name":"Donohue Exit 1","wpsName":"DonohueValley","region":null,"latitude":null,"longitude":null,"description":null,"quota":20,"capacity":20,"alert":null,"notes":null},"d02":{"id":"d02","name":"Donohue Exit 2","wpsName":"DonohueLyell","region":null,"latitude":null,"longitude":null,"description":null,"quota":15,"capacity":25,"alert":null,"notes":null},"x04":{"id":"x04","name":"Gaylor Creek (cross-country only)","wpsName":"Gaylor Creek\/Lake (cross-country only)","region":"tm","latitude":null,"longitude":null,"description":null,"quota":3,"capacity":3,"alert":"This is a cross-country trailhead, and the trail\/route is not maintained. All members of the party must be proficient at backcountry navigation.","notes":"<li>No camping in the park from Great Sierra Mine to White Mountain and all watershed downstream, including Gaylor Lakes.<\/li><li>No camping in the Monroe Hall Research Area.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"w03b":{"id":"w03b","name":"Glacier Point to Illilouette","wpsName":"Glacier Point->Illilouette","region":"ww","latitude":null,"longitude":null,"description":null,"quota":15,"capacity":25,"alert":null,"notes":"<li>Please remember you must travel as far as the Buena Vista Trail junction before camping.<\/li><li>You may not camp in Little Yosemite Valley your first night with this permit.<\/li><li>Camping is not allowed along the Panorama Trail or at the top of Illilouette Fall.<\/li>"},"w03a":{"id":"w03a","name":"Glacier Point to Little Yosemite Valley","wpsName":"Glacier Point->Little Yosemite Valley","region":"ww","latitude":null,"longitude":null,"description":"Trailhead availability does not reflect availability for <span>Donohue Pass<\/span> or <span>Half Dome Cables.<\/span> <span>John Muir Trail<\/span> hikers, please <a href=\"?region=jm&th=j03a\">click here<\/a>.","quota":6,"capacity":10,"alert":null,"notes":"<li>You must get to Little Yosemite Valley before camping. You must camp your first night at Little Yosemite Valley.<\/li><li>Bear boxes and composting toilet are available at the campground. Bears have obtained food from backpackers in this area.<\/li>"},"j03a":{"id":"j03a","name":"Glacier Point to Little Yosemite Valley","wpsName":"Glacier Point->Little Yosemite Valley","region":"jm","latitude":null,"longitude":null,"description":"If you do not plan on exiting the park via Donohue Pass, please <span><a href=\"?region=yv&th=y03a\">click here<\/a>.<\/span>","quota":6,"capacity":10,"alert":null,"notes":"<li>You must get to Little Yosemite Valley before camping. You must camp your first night at Little Yosemite Valley.<\/li><li>Bear boxes and composting toilets are there for your use.<\/li>"},"y03a":{"id":"y03a","name":"Glacier Point to Little Yosemite Valley","wpsName":"Glacier Point->Little Yosemite Valley","region":"yv","latitude":null,"longitude":null,"description":"Trailhead availability does not reflect availability for <span>Donohue Pass<\/span> or <span>Half Dome Cables.<\/span> <span>John Muir Trail<\/span> hikers, please <a href=\"?region=jm&th=j03a\">click here<\/a>.","quota":6,"capacity":10,"alert":null,"notes":"<li>You must get to Little Yosemite Valley before camping. You must camp your first night at Little Yosemite Valley.<\/li><li>Bear boxes and composting toilets are there for your use.<\/li>"},"t22a":{"id":"t22a","name":"Glen Aulin","wpsName":"Glen Aulin","region":"tm","latitude":null,"longitude":null,"description":null,"quota":15,"capacity":20,"alert":"The composting toilet at the Glen Aulin campground is not open this year. Please disperse several hundred feet from the campground boundary when going to the bathroom.","notes":"<li>You must camp at the Glen Aulin High Sierra Camp backpackers campground your first night.<\/li><li>Bears are very active and have obtained food from backpackers in this area.<\/li><li>Fires are permitted only in the established community fire rings.<\/li><li>The Tuolumne River is a water source for San Francisco. Follow Leave No Trace rules.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"t22b":{"id":"t22b","name":"Glen Aulin Pass Thru to Cold Canyon or Waterwheel Falls","wpsName":"Glen Aulin->Cold Canyon\/Waterwheel (pass through)","region":"tm","latitude":null,"longitude":null,"description":null,"quota":12,"capacity":16,"alert":"The composting toilet at the Glen Aulin campground is not open this year. Please disperse several hundred feet from the campground boundary when going to the bathroom.","notes":"<li>You may not camp at the Glen Aulin High Sierra Camp backpackers camp your first night with this permit.<\/li><li>Bears have been successful in getting food from backpackers in this area.<\/li><li>The Tuolumne River is a water source for San Francisco. Follow Leave No Trace camping techniques to preserve water quality.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"y01c":{"id":"y01c","name":"Happy Isles to Illilouette","wpsName":"Happy Isles->Illilouette","region":"yv","latitude":null,"longitude":null,"description":null,"quota":3,"capacity":5,"alert":null,"notes":"<li>You may not camp at Little Yosemite Valley your first night with this permit.<\/li>"},"y01b":{"id":"y01b","name":"Happy Isles to Little Yosemite Valley","wpsName":"Happy Isles->Little Yosemite Valley","region":"yv","latitude":null,"longitude":null,"description":"Trailhead availability does not reflect availability for <span>Donohue Pass<\/span> or <span>Half Dome Cables.<\/span> <span>John Muir Trail<\/span> hikers, please <a href=\"?region=jm&th=j01b\">click here<\/a>.","quota":18,"capacity":30,"alert":null,"notes":"<li>You must get to Little Yosemite Valley before camping. You must camp your first night at Little Yosemite Valley.<\/li><li>Bear boxes and composting toilet are available at the campground. Bears have obtained food from backpackers in this area.<\/li>"},"j01b":{"id":"j01b","name":"Happy Isles to Little Yosemite Valley","wpsName":"Happy Isles->Little Yosemite Valley","region":"jm","latitude":null,"longitude":null,"description":"If you do not plan on exiting the park via Donohue Pass, please <span><a href=\"?region=yv&th=y01b\">click here<\/a>.<\/span>","quota":18,"capacity":30,"alert":null,"notes":"<li>You must get to Little Yosemite Valley before camping. You must camp your first night at Little Yosemite Valley.<\/li><li>Bear boxes and composting toilet are available at the campground.<\/li>"},"y01a":{"id":"y01a","name":"Happy Isles to Sunrise\/Merced Lake Pass Thru","wpsName":"Happy Isles->Sunrise\/Merced Lake (pass through)","region":"yv","latitude":null,"longitude":null,"description":"Trailhead availability does not reflect availability for <span>Donohue Pass<\/span> or <span>Half Dome Cables.<\/span> <span>John Muir Trail<\/span> hikers, please <a href=\"?region=jm&th=j01a\">click here<\/a>.","quota":6,"capacity":10,"alert":null,"notes":"<li>You must camp beyond Little Yosemite Valley and Moraine Dome.<\/li><li>Bears have obtained food from backpackers in this area. There are bear lockers at Merced Lake backpackers camp.<\/li>"},"j01a":{"id":"j01a","name":"Happy Isles to Sunrise\/Merced Lake Pass Thru","wpsName":"Happy Isles->Sunrise\/Merced Lake (pass through)","region":"jm","latitude":null,"longitude":null,"description":"If you do not plan on exiting the park via Donohue Pass, please <span><a href=\"?region=yv&th=y01a\">click here<\/a>.<\/span>","quota":6,"capacity":10,"alert":null,"notes":"<li>You must camp beyond Little Yosemite Valley and Moraine Dome.<\/li><li>Bears have obtained food from backpackers in this area. There are bear lockers at Merced Lake backpackers camp.<\/li>"},"b13b":{"id":"b13b","name":"Luken to Luken's Lake","wpsName":"Luken->Lukens Lake","region":"bf","latitude":null,"longitude":null,"description":null,"quota":6,"capacity":10,"alert":null,"notes":"<li>Camping is not permitted at Luken's Lake.<\/li>"},"b13a":{"id":"b13a","name":"Luken to Yosemite Creek","wpsName":"Lukens Lake->Yosemite Creek","region":"bf","latitude":null,"longitude":null,"description":null,"quota":6,"capacity":10,"alert":"This trailhead is currently <b>closed<\/b> due to the <span><a href=\"https:\/\/inciweb.nwcg.gov\/incident\/6888\/\" target=\"_blank\">Blue Jay Fire<\/a>.<\/span> <span><a href=\"https:\/\/www.nps.gov\/yose\/learn\/management\/closures.htm#cs_control_6605255\" target=\"_blank\">Learn more<\/a>.<\/span>","notes":"<li>Camp at least one-half mile back from the rim of the Valley.<\/li><li>The area around the top of Yosemite Falls is for day use only.<\/li>"},"j24b":{"id":"j24b","name":"Lyell Canyon","wpsName":"Lyell Canyon","region":"jm","latitude":null,"longitude":null,"description":"If you do not plan on exiting the park via Donohue Pass, please <span><a href=\"?region=tm&th=t24b\">click here<\/a>.<\/span>","quota":21,"capacity":35,"alert":null,"notes":"<li>Travel at least four miles out Lyell Canyon before camping.<\/li><li>Bears have been successful in getting food from backpackers in this area. Bear canisters are required.<\/li><li>No fires above 9,600 feet.<\/li><li>The Tuolumne River is a water source for San Francisco. Follow Leave No Trace rules.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"t24b":{"id":"t24b","name":"Lyell Canyon","wpsName":"Lyell Canyon","region":"tm","latitude":null,"longitude":null,"description":"Trailhead availability does not reflect availability for <span>Donohue Pass.<\/span> <span>John Muir Trail<\/span> hikers, please <a href=\"?region=jm&th=j24b\">click here<\/a>.","quota":21,"capacity":35,"alert":null,"notes":"<li>Travel at least four miles out Lyell Canyon before camping.<\/li><li>Bears have been successful in getting food from backpackers in this area. Bear canisters are required.<\/li><li>No fires above 9,600 feet.<\/li><li>The Tuolumne River is a water source for San Francisco. Follow Leave No Trace rules.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"h26b":{"id":"h26b","name":"Mather Ranger Station","wpsName":"Mather Ranger Station","region":"hh","latitude":null,"longitude":null,"description":null,"quota":15,"capacity":25,"alert":null,"notes":null},"b17":{"id":"b17","name":"May Lake","wpsName":"May Lake","region":"bf","latitude":null,"longitude":null,"description":null,"quota":15,"capacity":25,"alert":null,"notes":"<li>All food and toiletries must be stored in proper food storage containers.<\/li><li>Only use existing fire rings.<\/li><li>Pack out all trash, including toilet paper.<\/li>"},"b16":{"id":"b16","name":"May Lake to Snow Creek","wpsName":"May Lake->Snow Creek","region":"bf","latitude":null,"longitude":null,"description":null,"quota":6,"capacity":10,"alert":"An area closure is in effect to the south and east of the Snow Creek footbridge. <span><a href=\"https:\/\/www.nps.gov\/yose\/learn\/management\/closures.htm#cs_control_5560287\" target=\"_blank\">Learn more<\/a>.<\/span>","notes":"<li>Bears are active in this area. Do not place your bear canister near a cliff.<\/li>"},"w31a":{"id":"w31a","name":"McGurk Meadow","wpsName":"McGurk  Meadow","region":"ww","latitude":null,"longitude":null,"description":null,"quota":9,"capacity":15,"alert":null,"notes":"<li>You must be four trail miles from Glacier Point before camping. No camping east of the Bridalveil Creek footbridge.<\/li>"},"h29a":{"id":"h29a","name":"Miguel Meadows","wpsName":"Miguel Meadows","region":"hh","latitude":null,"longitude":null,"description":null,"quota":9,"capacity":15,"alert":null,"notes":null},"y02":{"id":"y02","name":"Mirror Lake to Snow Creek","wpsName":"Mirror Lake->Snow Creek","region":"yv","latitude":null,"longitude":null,"description":null,"quota":15,"capacity":25,"alert":"An area closure is in effect to the south and east of the Snow Creek footbridge. <span><a href=\"https:\/\/www.nps.gov\/yose\/learn\/management\/closures.htm#cs_control_5560287\" target=\"_blank\">Learn more<\/a>.<\/span>","notes":"<li>You must camp beyond the top of the switchbacks and outside of the closure area to the south and east of the Snow Creek footbridge.<\/li><li>Bears are active in this area. Do not put your bear canister next to a cliff.<\/li>"},"w34":{"id":"w34","name":"Mono Meadow","wpsName":"Mono Meadow","region":"ww","latitude":null,"longitude":null,"description":null,"quota":12,"capacity":20,"alert":null,"notes":"<li>You may not camp in Little Yosemite Valley your first night with this permit.<\/li><li>Camping is not allowed along the Panorama Trail or at the top of Illilouette Fall.<\/li>"},"t25":{"id":"t25","name":"Mono\/Parker Pass","wpsName":"Mono\/Parker Pass","region":"tm","latitude":null,"longitude":null,"description":null,"quota":9,"capacity":15,"alert":null,"notes":"<li>Travel across Mono Pass before camping.<\/li><li>The Parker Pass Creek watershed is closed to camping, travel over Parker Pass before camping.<\/li><li>The Tuolumne River is a water source for San Francisco. Follow Leave No Trace techniques to preserve water quality.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"t20":{"id":"t20","name":"Murphy Creek","wpsName":"Murphy Creek","region":"tm","latitude":null,"longitude":null,"description":null,"quota":9,"capacity":15,"alert":null,"notes":"<li>Bears have been successful in getting food from backpackers in this area. All food, toiletries, aromatic goods, and garbage must be stored in the canister.<\/li>"},"x02":{"id":"x02","name":"Nelson Lake (cross-country only)","wpsName":"Nelson Lake (cross-country only)","region":"tm","latitude":null,"longitude":null,"description":null,"quota":9,"capacity":15,"alert":"This is a cross-country trailhead, and the trail\/route is not maintained. All members of the party must be proficient at backcountry navigation.","notes":"<li>Cross-country restrictions are in effect. The maximum group size is 8. <span><a href=\"https:\/\/www.nps.gov\/yose\/planyourvisit\/backpackinggroups.htm\" target=\"_blank\">Learn more<\/a>.<\/span><\/li><li>Fires are prohibited at Nelson and Reyman Lakes.<\/li><li>Camping is prohibited at Elizabeth Lake.<\/li><li>Bears are active in this area.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"y08":{"id":"y08","name":"Old Big Oak Flat Road","wpsName":"Old Big Oak Flat Road","region":"yv","latitude":null,"longitude":null,"description":null,"quota":6,"capacity":10,"alert":null,"notes":"<li>Camp at least one-half mile back from the rim of the Valley.<\/li><li>The area around the top of the falls is for day use only.<\/li>"},"w33":{"id":"w33","name":"Ostrander Lake","wpsName":"Ostrander (Lost Bear Meadow)","region":"ww","latitude":null,"longitude":null,"description":null,"quota":15,"capacity":25,"alert":null,"notes":"<li>All food and toiletries must be stored in proper food storage containers.<\/li><li>Only use existing fire rings.<\/li><li>Pack out all trash, including toilet paper.<\/li>"},"w03c":{"id":"w03c","name":"Pohono Trail (Glacier Point)","wpsName":"Pohono Trail (Glacier Point)","region":"ww","latitude":null,"longitude":null,"description":null,"quota":9,"capacity":15,"alert":null,"notes":"<li>You must be four trail miles from Glacier Point before camping. No camping east of the Bridalveil Creek footbridge.<\/li>"},"w05":{"id":"w05","name":"Pohono Trail (Taft Point)","wpsName":"Pohono Trail (Taft Point)","region":"ww","latitude":null,"longitude":null,"description":null,"quota":6,"capacity":10,"alert":null,"notes":"<li>You must be four trail miles from Glacier Point before camping. No camping east of the Bridalveil Creek footbridge including Taft Point.<\/li>"},"y07":{"id":"y07","name":"Pohono Trail (Wawona Tunnel)","wpsName":"Pohono Trail (Wawona Tunnel\/Bridalveil Parking)","region":"yv","latitude":null,"longitude":null,"description":null,"quota":6,"capacity":10,"alert":null,"notes":"<li>No camping east of the Bridalveil Creek footbridge. Only use existing fire rings.<\/li>"},"h27":{"id":"h27","name":"Poopenaut Valley","wpsName":"Poopenaut Valley","region":"hh","latitude":null,"longitude":null,"description":null,"quota":15,"capacity":25,"alert":null,"notes":"<li>Bears have been successful in getting food from backpackers in the Hetch Hetchy area.<\/li><li>The Tuolumne River is a water source for San Francisco. Follow Leave No Trace camping techniques to preserve water quality.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"b15":{"id":"b15","name":"Porcupine Creek","wpsName":"Porcupine Creek","region":"bf","latitude":null,"longitude":null,"description":null,"quota":12,"capacity":20,"alert":null,"notes":"<li>Bears are active in this area. Do not place your bear canister near a cliff.<\/li>"},"t24a":{"id":"t24a","name":"Rafferty Creek to Vogelsang","wpsName":"Rafferty Creek->Vogelsang","region":"tm","latitude":null,"longitude":null,"description":null,"quota":12,"capacity":20,"alert":"There is no longer a toilet at the Vogelsang backpackers campground near Fletcher Lake. If staying at Fletcher Lake please disperse several hunderd feet from the campground boundary when going to the bathroom.","notes":"<li>Fires are prohibited in the Vogesang area, at Boothe Lake, and above 9,600 feet.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"h29c":{"id":"h29c","name":"Rancheria Falls","wpsName":"Rancheria Falls","region":"hh","latitude":null,"longitude":null,"description":null,"quota":21,"capacity":35,"alert":null,"notes":"<li>Swimming and watering of stock directly in streams within one mile of the Reservoir is prohibited. Closure includes Wapama Falls, Rancheria Cascade.<\/li><li>Bears active in Rancheria area.<\/li><li>The Tuolumne River is water source for San Francisco. Please preserve water quality.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"x01":{"id":"x01","name":"Rockslides (cross-country only)","wpsName":"Rockslides (cross-country only)","region":"yv","latitude":null,"longitude":null,"description":null,"quota":6,"capacity":10,"alert":"This is a cross-country trailhead, and the trail\/route is not maintained. All members of the party must be proficient at backcountry navigation.","notes":"<li>Cross-country restrictions are in effect. The maximum group size is 8. <span><a href=\"https:\/\/www.nps.gov\/yose\/planyourvisit\/backpackinggroups.htm\" target=\"_blank\">Learn more<\/a>.<\/span><\/li><li>Camp at least one-half mile back from the rim of the Valley.<\/li><li>The area around the top of the falls is for day use only.<\/li>"},"h28":{"id":"h28","name":"Smith Peak","wpsName":"Smith Peak","region":"hh","latitude":null,"longitude":null,"description":null,"quota":9,"capacity":15,"alert":null,"notes":"<li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"b10":{"id":"b10","name":"South Fork of Tuolumne River","wpsName":"South Fork of Tuolumne River","region":"bf","latitude":null,"longitude":null,"description":null,"quota":15,"capacity":25,"alert":null,"notes":"<li>The Tuolumne River is a water source for San Francisco. Follow Leave No Trace camping techniques to preserve water quality.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"t19":{"id":"t19","name":"Sunrise Lakes","wpsName":"Sunrise Lakes","region":"tm","latitude":null,"longitude":null,"description":"Trailhead availability does not reflect availability for <span>Donohue Pass.<\/span> <span>John Muir Trail<\/span> hikers, please <a href=\"?region=jm&th=j19\">click here<\/a>.","quota":9,"capacity":15,"alert":null,"notes":"<li>Bears have successfully obtained food from backpackers in this area on a regular basis. Store all food and toiletries in a bear canister.<\/li><li>Camp at least 100 feet from any water source.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"j19":{"id":"j19","name":"Sunrise Lakes","wpsName":"Sunrise Lakes","region":"jm","latitude":null,"longitude":null,"description":"If you do not plan on exiting the park via Donohue Pass, please <span><a href=\"?region=tm&th=t19\">click here<\/a>.<\/span>","quota":9,"capacity":15,"alert":null,"notes":"<li>Bears have successfully obtained food from backpackers in this area on a regular basis. Store all food and toiletries in a bear canister.<\/li><li>Camp at least 100 feet from any water source.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"y09":{"id":"y09","name":"Tamarack Creek","wpsName":"Tamarack Creek","region":"yv","latitude":null,"longitude":null,"description":null,"quota":15,"capacity":25,"alert":null,"notes":"<li>You may not camp at Tamarack Flat Campground with this permit.<\/li><li>Camp at least one-half mile back from the rim of the Valley.<\/li><li>The area around the top of Yosemite Falls is for day use only.<\/li>"},"b14b":{"id":"b14b","name":"Ten Lakes","wpsName":"Ten Lakes","region":"bf","latitude":null,"longitude":null,"description":null,"quota":24,"capacity":40,"alert":null,"notes":"<li>Fires are not allowed above 9600 feet and are permitted only in existing fire rings below that elevation.<\/li><li>Bears are active in this area.<\/li><li>The Tuolumne River is a water source for San Francisco. Follow Leave No Trace camping techniques to preserve water quality.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"w31b":{"id":"w31b","name":"Westfall Meadows","wpsName":"Westfall Meadows","region":"ww","latitude":null,"longitude":null,"description":null,"quota":9,"capacity":15,"alert":null,"notes":null},"b12b":{"id":"b12b","name":"White Wolf Campground","wpsName":"White Wolf Campground","region":"bf","latitude":null,"longitude":null,"description":null,"quota":6,"capacity":10,"alert":"The road to White Wolf is closed. You may access the trailhead from Tioga Road.","notes":"<li>You may not camp at White Wolf Campground with this permit.<\/li><li>The Tuolumne River watershed is a water source for San Francisco. Follow Leave No Trace camping techniques to preserve water quality.<\/li>"},"b12c":{"id":"b12c","name":"White Wolf to Aspen Valley","wpsName":"White Wolf->Aspen Valley","region":"bf","latitude":null,"longitude":null,"description":null,"quota":15,"capacity":25,"alert":"The road to White Wolf is closed. You may access the trailhead from Tioga Road.","notes":"<li>This trail is not used often and portions of the trail are overgrown with vegetation. Bring a good map of the area.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"b12a":{"id":"b12a","name":"White Wolf to Pate Valley","wpsName":"White Wolf->Pate Valley","region":"bf","latitude":null,"longitude":null,"description":null,"quota":18,"capacity":30,"alert":"The road to White Wolf is closed. You may access the trailhead from Tioga Road.","notes":"<li>Bears are active in Pate Valley.<\/li><li>The Tuolumne River is a water source for San Francisco. Follow Leave No Trace camping techniques to preserve water quality.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"b12d":{"id":"b12d","name":"White Wolf to Smith Meadow","wpsName":"White Wolf->Smith Meadow (including Harden Lake)","region":"bf","latitude":null,"longitude":null,"description":null,"quota":15,"capacity":25,"alert":"The road to White Wolf is closed. You may access the trailhead from Tioga Road.","notes":"<li>This trail is not used often and portions of the trail are overgrown with vegetation. Bring a good map of the area.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"b14a":{"id":"b14a","name":"Yosemite Creek","wpsName":"Yosemite Creek","region":"bf","latitude":null,"longitude":null,"description":null,"quota":15,"capacity":25,"alert":null,"notes":"<li>Camp at least one-half mile back from the rim of the Valley.<\/li><li>The area around the top of Yosemite Falls is for day use only.<\/li>"},"y06":{"id":"y06","name":"Yosemite Falls","wpsName":"Yosemite Falls","region":"yv","latitude":null,"longitude":null,"description":null,"quota":15,"capacity":25,"alert":null,"notes":"<li>Camp at least one-half mile back from the rim of the Valley. <\/li><li>The area around the top of Yosemite Falls and around Yosemite Point is for day use only.<\/li>"},"t23":{"id":"t23","name":"Young Lakes via Dog Lake","wpsName":"Young Lakes via Dog Lake","region":"tm","latitude":null,"longitude":null,"description":null,"quota":12,"capacity":20,"alert":null,"notes":"<li>Fires are not allowed at Young Lakes or anywhere in the park above 9,600 feet.<\/li><li>Bears are active in this area. The Tuolumne River is a water source for San Francisco. Follow Leave No Trace camping techniques to preserve water quality.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"},"t22c":{"id":"t22c","name":"Young Lakes via Glen Aulin Trail","wpsName":"Young Lakes via Glen Aulin Trail","region":"tm","latitude":null,"longitude":null,"description":null,"quota":6,"capacity":10,"alert":null,"notes":"<li>Fires are not allowed at Young Lakes or anywhere in the park above 9,600 feet.<\/li><li>Bears are active in this area.<\/li><li>The Tuolumne River is a water source for San Francisco. Follow Leave No Trace camping techniques to preserve water quality.<\/li><li>Along the Tuolumne Watershed, ensure all washing and waste is 300 feet from water.<\/li>"
            }}}
        }"#;
        let res = serde_json::from_str::<Response<Trailheads>>(test);
        let resp = res.expect("derp");
        println!("{:?}", resp)
    }

    #[test]
    fn parse_report() {
        let test = r#"{
            "status":{"type":"message","value":"report found."},
            "response":
                {"id":"bf","values":[
                    {"date":"2020-09-10","b10":0,"b12a":17,"b12b":6,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":11,"b14b":26,"b15":16,"b16":6,"b17":22},{"date":"2020-09-11","b10":4,"b12a":22,"b12b":5,"b12c":0,"b12d":0,"b13a":2,"b13b":10,"b14a":4,"b14b":40,"b15":18,"b16":9,"b17":25},{"date":"2020-09-12","b10":8,"b12a":25,"b12b":0,"b12c":0,"b12d":0,"b13a":7,"b13b":8,"b14a":11,"b14b":40,"b15":20,"b16":10,"b17":25},{"date":"2020-09-13","b10":6,"b12a":10,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":4,"b14b":26,"b15":18,"b16":0,"b17":25},{"date":"2020-09-14","b10":0,"b12a":11,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":2,"b14a":0,"b14b":16,"b15":3,"b16":0,"b17":20},{"date":"2020-09-15","b10":0,"b12a":6,"b12b":0,"b12c":0,"b12d":0,"b13a":4,"b13b":0,"b14a":2,"b14b":14,"b15":8,"b16":0,"b17":21},{"date":"2020-09-16","b10":0,"b12a":18,"b12b":4,"b12c":2,"b12d":5,"b13a":0,"b13b":0,"b14a":2,"b14b":13,"b15":11,"b16":2,"b17":17},{"date":"2020-09-17","b10":0,"b12a":24,"b12b":2,"b12c":0,"b12d":2,"b13a":4,"b13b":6,"b14a":9,"b14b":32,"b15":16,"b16":6,"b17":24},{"date":"2020-09-18","b10":0,"b12a":25,"b12b":2,"b12c":0,"b12d":2,"b13a":2,"b13b":10,"b14a":5,"b14b":34,"b15":20,"b16":10,"b17":22},{"date":"2020-09-19","b10":2,"b12a":19,"b12b":0,"b12c":0,"b12d":8,"b13a":0,"b13b":7,"b14a":12,"b14b":39,"b15":20,"b16":9,"b17":24},{"date":"2020-09-20","b10":0,"b12a":21,"b12b":4,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":5,"b14b":25,"b15":16,"b16":0,"b17":22},{"date":"2020-09-21","b10":0,"b12a":2,"b12b":0,"b12c":0,"b12d":3,"b13a":0,"b13b":0,"b14a":0,"b14b":31,"b15":11,"b16":0,"b17":16},{"date":"2020-09-22","b10":0,"b12a":4,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":14,"b14b":24,"b15":15,"b16":6,"b17":17},{"date":"2020-09-23","b10":0,"b12a":4,"b12b":2,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":3,"b14b":16,"b15":4,"b16":0,"b17":13},{"date":"2020-09-24","b10":0,"b12a":2,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":6,"b14b":24,"b15":12,"b16":0,"b17":15},{"date":"2020-09-25","b10":0,"b12a":20,"b12b":4,"b12c":0,"b12d":4,"b13a":6,"b13b":6,"b14a":15,"b14b":25,"b15":10,"b16":6,"b17":15},{"date":"2020-09-26","b10":0,"b12a":4,"b12b":0,"b12c":0,"b12d":0,"b13a":4,"b13b":4,"b14a":8,"b14b":27,"b15":11,"b16":6,"b17":15},{"date":"2020-09-27","b10":0,"b12a":10,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":12,"b15":9,"b16":0,"b17":15},{"date":"2020-09-28","b10":0,"b12a":4,"b12b":0,"b12c":0,"b12d":0,"b13a":1,"b13b":2,"b14a":0,"b14b":0,"b15":5,"b16":1,"b17":15},{"date":"2020-09-29","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":14,"b15":2,"b16":0,"b17":5},{"date":"2020-09-30","b10":0,"b12a":8,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":2,"b14b":11,"b15":8,"b16":0,"b17":5},{"date":"2020-10-01","b10":0,"b12a":10,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":3,"b14a":0,"b14b":22,"b15":9,"b16":0,"b17":15},{"date":"2020-10-02","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":6,"b14b":24,"b15":18,"b16":0,"b17":14},{"date":"2020-10-03","b10":0,"b12a":0,"b12b":2,"b12c":0,"b12d":0,"b13a":4,"b13b":0,"b14a":6,"b14b":23,"b15":11,"b16":5,"b17":15},{"date":"2020-10-04","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":4},{"date":"2020-10-05","b10":0,"b12a":4,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":2,"b15":2,"b16":0,"b17":0},{"date":"2020-10-06","b10":0,"b12a":2,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-07","b10":0,"b12a":2,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":2,"b14a":0,"b14b":3,"b15":3,"b16":0,"b17":0},{"date":"2020-10-08","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":2,"b13b":0,"b14a":0,"b14b":11,"b15":12,"b16":0,"b17":3},{"date":"2020-10-09","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":4,"b13b":0,"b14a":0,"b14b":10,"b15":8,"b16":0,"b17":14},{"date":"2020-10-10","b10":0,"b12a":4,"b12b":0,"b12c":0,"b12d":0,"b13a":3,"b13b":0,"b14a":14,"b14b":17,"b15":12,"b16":0,"b17":15},{"date":"2020-10-11","b10":0,"b12a":5,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":8,"b16":5,"b17":10},{"date":"2020-10-12","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":2,"b15":2,"b16":0,"b17":0},{"date":"2020-10-13","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-14","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-15","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-16","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-17","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-18","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-19","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-20","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-21","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-22","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-23","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-24","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-25","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-26","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-27","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-28","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-29","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-30","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0},{"date":"2020-10-31","b10":0,"b12a":0,"b12b":0,"b12c":0,"b12d":0,"b13a":0,"b13b":0,"b14a":0,"b14b":0,"b15":0,"b16":0,"b17":0}],"timestamp":"2020-09-09T13:12:44"}}
        "#;

        let res = serde_json::from_str::<Response<Report>>(test);
        let resp = res.expect("derp");
        println!("{:?}", resp)
    }
}
