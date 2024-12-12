use std::fs::File;
use std::io::{self, BufReader, Write};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use serde_json::from_reader;
use geo::{prelude::*, Point};
use geojson::{Feature, GeoJson, Value};
use anyhow::{Result, Context};

const MAX_MARGIN_INCREASE: f64 = 100.0;
const MARGIN_STEP: f64 = 1.0;

#[derive(Debug)]
struct DistanceCache {
    cache: HashMap<(String, String), f64>,
}

impl DistanceCache {
    fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    fn get_or_calculate<F>(&mut self, country1: &str, country2: &str, calc_fn: F) -> Option<f64>
    where
        F: FnOnce() -> Option<f64>,
    {
        let key = if country1 < country2 {
            (country1.to_string(), country2.to_string())
        } else {
            (country2.to_string(), country1.to_string())
        };

        if let Some(&distance) = self.cache.get(&key) {
            Some(distance)
        } else {
            let distance = calc_fn()?;
            self.cache.insert(key, distance);
            Some(distance)
        }
    }
}

#[derive(Clone)]
struct CountryData {
    name: String,
    points: Vec<Point<f64>>,
}

fn main() -> Result<()> {
    println!("Country Distance Calculator");
    println!("==========================");

    let file = File::open("country_data.json").context("Failed to open country_data.json")?;
    let reader = BufReader::new(file);
    let geojson: GeoJson = from_reader(reader).context("Invalid GeoJSON format")?;

    let countries = match geojson {
        GeoJson::FeatureCollection(fc) => fc.features,
        _ => anyhow::bail!("GeoJSON is not a FeatureCollection"),
    };

    // Pre-process all country geometries
    let country_geometries: Vec<CountryData> = countries.iter()
        .filter_map(|country| {
            let name = country.properties.as_ref()?
                .get("NAME")?
                .as_str()?
                .to_string();
            let points = extract_points(country)?;
            Some(CountryData { name, points })
        })
        .collect();

    let cache = Arc::new(Mutex::new(DistanceCache::new()));

    loop {
        print!("\nEnter the country you guessed (or 'quit' to exit): ");
        io::stdout().flush()?;
        let mut guessed_country_name = String::new();
        io::stdin().read_line(&mut guessed_country_name)?;
        let guessed_country_name = guessed_country_name.trim();

        if guessed_country_name.eq_ignore_ascii_case("quit") {
            println!("Thank you for using the Country Distance Calculator!");
            break;
        }

        let guessed_country = match country_geometries.iter().find(|c| {
            c.name.eq_ignore_ascii_case(guessed_country_name)
        }) {
            Some(country) => country,
            None => {
                println!("Error: Country '{}' not found in database", guessed_country_name);
                continue;
            }
        };

        print!("Enter the distance (km) and optional margin (e.g., 500--50): ");
        io::stdout().flush()?;
        let mut distance_input = String::new();
        io::stdin().read_line(&mut distance_input)?;

        let (known_distance_km, initial_margin) = match parse_distance_input(&distance_input) {
            Ok(result) => result,
            Err(e) => {
                println!("Error parsing distance: {}", e);
                continue;
            }
        };

        let mut margin_error_km = initial_margin;
        let mut possible_countries;

        loop {
            possible_countries = find_mystery_countries(
                guessed_country,
                known_distance_km,
                margin_error_km,
                &country_geometries,
                Arc::clone(&cache)
            );

            if !possible_countries.is_empty() || margin_error_km >= MAX_MARGIN_INCREASE {
                break;
            }

            margin_error_km += MARGIN_STEP;
            println!("No countries found, increasing search margin to {} km...", margin_error_km);
        }

        if possible_countries.is_empty() {
            println!("\nNo countries found even with increased margin of {} km.", margin_error_km);
        } else {
            if margin_error_km > initial_margin {
                println!("\nFound countries with adjusted margin of {} km:", margin_error_km);
            }
            println!("\nPossible mystery countries ({} found):", possible_countries.len());
            for country_name in possible_countries {
                println!("- {}", country_name);
            }
        }
    }

    Ok(())
}

fn parse_distance_input(input: &str) -> Result<(f64, f64)> {
    let input = input.trim();
    let parts: Vec<&str> = input.split("--").collect();

    match parts.len() {
        1 => {
            let distance = parts[0].parse().context("Invalid distance format")?;
            if distance < 0.0 {
                anyhow::bail!("Distance cannot be negative");
            }
            Ok((distance, 0.0))
        }
        2 => {
            let distance = parts[0].parse().context("Invalid distance format")?;
            let margin = parts[1].parse().context("Invalid margin format")?;
            if distance < 0.0 || margin < 0.0 {
                anyhow::bail!("Distance and margin must be non-negative");
            }
            Ok((distance, margin))
        }
        _ => anyhow::bail!("Invalid input format. Use 'distance' or 'distance--margin'")
    }
}

fn find_mystery_countries(
    guessed_country: &CountryData,
    known_distance_km: f64,
    margin_error_km: f64,
    all_countries: &[CountryData],
    cache: Arc<Mutex<DistanceCache>>,
) -> Vec<String> {
    let lower_bound = known_distance_km - margin_error_km;
    let upper_bound = known_distance_km + margin_error_km;

    all_countries.iter()
        .filter(|country| country.name != guessed_country.name)
        .filter_map(|country| {
            if is_special_case(&guessed_country.name, &country.name) {
                return Some(country.name.clone());
            }

            let mut cache_guard = cache.lock().ok()?;
            let distance_km = cache_guard.get_or_calculate(
                &guessed_country.name,
                &country.name,
                || Some(calculate_min_distance_km(&guessed_country.points, &country.points))
            )?;
            drop(cache_guard);

            if distance_km >= lower_bound && distance_km <= upper_bound {
                Some(country.name.clone())
            } else {
                None
            }
        })
        .collect()
}

fn is_special_case(country1: &str, country2: &str) -> bool {
    let special_pairs = [
        ("South Africa", "Lesotho"),
        ("Italy", "Vatican"),
        ("Italy", "San Marino"),
        ("France", "Monaco"),
        ("Spain", "Gibraltar"),
        ("China", "Hong Kong"),
        ("China", "Macau"),
    ];

    special_pairs.iter().any(|&(a, b)| {
        (country1.eq_ignore_ascii_case(a) && country2.eq_ignore_ascii_case(b)) ||
            (country1.eq_ignore_ascii_case(b) && country2.eq_ignore_ascii_case(a))
    })
}

fn extract_points(country: &Feature) -> Option<Vec<Point<f64>>> {
    let geometry = country.geometry.as_ref()?;
    let mut points = Vec::with_capacity(100);

    match &geometry.value {
        Value::MultiPolygon(coords) => {
            for polygon in coords {
                for ring in polygon {
                    points.extend(ring.iter().map(|coord| Point::new(coord[0], coord[1])));
                }
            }
        }
        Value::Polygon(coords) => {
            for ring in coords {
                points.extend(ring.iter().map(|coord| Point::new(coord[0], coord[1])));
            }
        }
        _ => return None,
    }
    Some(points)
}

fn calculate_min_distance_km(points1: &[Point<f64>], points2: &[Point<f64>]) -> f64 {
    points1.iter()
        .flat_map(|p1| points2.iter().map(move |p2| p1.haversine_distance(p2)))
        .fold(f64::INFINITY, f64::min) / 1000.0
}