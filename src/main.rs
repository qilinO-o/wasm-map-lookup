use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use std::fs;

#[derive(Parser, Debug)]
#[command(about = "Lookup TS source position by WASM binary offset using AS source map")]
struct Args {
    /// Path to the .wasm.map JSON file
    map: String,
    /// One or more target WASM offsets (decimal or 0x hex). Accepts multiple values.
    offsets: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[allow(unused)]
struct SourceMap {
    version: u32,
    sources: Vec<String>,
    #[serde(default)]
    names: Vec<String>,
    mappings: String,
}

fn parse_offset(s: &str) -> Option<u32> {
    if s.starts_with("0x") || s.starts_with("0X") {
        u32::from_str_radix(&s[2..], 16).ok()
    } else {
        s.parse::<u32>().ok()
    }
}

fn vlq_decode(segment: &str) -> Vec<i32> {
    let mut result = Vec::new();
    let mut value = 0i32;
    let mut shift = 0;
    for c in segment.chars() {
        let mut digit = match c {
            'A'..='Z' => (c as u8 - b'A') as i32,
            'a'..='z' => (c as u8 - b'a' + 26) as i32,
            '0'..='9' => (c as u8 - b'0' + 52) as i32,
            '+' => 62,
            '/' => 63,
            _ => continue,
        };
        let continuation = (digit & 32) != 0;
        digit &= 31;
        value += digit << shift;
        shift += 5;
        if !continuation {
            let sign = if (value & 1) != 0 { -1 } else { 1 };
            let val = sign * (value >> 1);
            result.push(val);
            value = 0;
            shift = 0;
        }
    }
    result
}

#[derive(Debug, Clone)]
struct MappingEntry {
    gen_offset: u32,
    source: Option<String>,
    line: Option<u32>,
    column: Option<u32>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    if args.offsets.is_empty() {
        anyhow::bail!("Please provide at least one offset to query (decimal or 0xhex).");
    }

    let target_offsets: Result<Vec<u32>> = args.offsets.iter().map(
        |s| parse_offset(s).ok_or_else(|| anyhow::anyhow!("Invalid offset"))
    ).collect();
    let target_offsets = target_offsets?;

    let data = fs::read_to_string(&args.map)
        .with_context(|| format!("Failed to read map file '{}'", &args.map))?;
    let sm: SourceMap = serde_json::from_str(&data)
        .with_context(|| format!("Failed to parse JSON from '{}'", &args.map))?;

    let mut entries: Vec<MappingEntry> = Vec::new();

    let mut gen_offset = 0u32;
    let mut source_index = 0i32;
    let mut original_line = 0i32;
    let mut original_column = 0i32;

    for line in sm.mappings.split(';') {
        if line.is_empty() { continue; }
        for segment in line.split(',') {
            let fields = vlq_decode(segment);
            if fields.is_empty() { continue; }
            let mut idx = 0;

            // generated column (Wasm offset)
            gen_offset = gen_offset.wrapping_add(fields[idx] as u32);
            idx += 1;

            let mut src = None;
            let mut orig_line = None;
            let mut orig_col = None;

            if fields.len() >= 4 {
                source_index += fields[idx]; idx += 1;
                src = sm.sources.get(source_index as usize).cloned();

                original_line += fields[idx]; idx += 1;
                orig_line = Some((original_line + 1) as u32); // line No. 1-based

                original_column += fields[idx]; // idx += 1;
                orig_col = Some(original_column as u32);
            }

            entries.push(MappingEntry {
                gen_offset,
                source: src,
                line: orig_line,
                column: orig_col,
            });
        }
    }

    if entries.is_empty() {
        anyhow::bail!("No mapping entries parsed from 'mappings' field. The map might not include VLQ mappings.");
    }

    // ascendant
    entries.sort_by_key(|e| e.gen_offset);

    for target_offset in target_offsets {
        get_source(&entries, target_offset);
    }

    Ok(())
}

fn get_source(entries: &Vec<MappingEntry>, target_offset: u32) {
    // bin search for the biggest offset <= target_offset
    let idx = match entries.binary_search_by(|e| e.gen_offset.cmp(&target_offset)) {
        Ok(i) => i,                 // precise
        Err(0) => {
            println!("No mapping found <= offset 0x{:x}", target_offset);
            return;
        }
        Err(i) => i - 1,            // not precise, the one before is that <= target
    };
    let best = entries.get(idx);

    match best {
        Some(e) => {
            println!("Query offset: 0x{:x}({}), Best match offset: 0x{:x}({})", target_offset, target_offset, e.gen_offset, e.gen_offset);
            if e.source.is_none() {
                // cannot find source, maybe runtime internally generated
                let prev_ts = entries[..idx].iter().rfind(|prev| prev.source.is_some());
                println!("Segment: (internal / runtime generated)");
                if let Some(ts) = prev_ts {
                    println!(
                        "Closest TS source before this: {}:{}:{}",
                        ts.source.as_deref().unwrap_or("(unknown)"),
                        ts.line.map(|n| n.to_string()).unwrap_or("?".to_string()),
                        ts.column.map(|n| n.to_string()).unwrap_or("?".to_string())
                    );
                } else {
                    println!("No previous TS source found");
                }
            } else {
                println!("Source: {}:{}:{}",
                    e.source.as_deref().unwrap_or("(no source)"),
                    e.line.map(|n| n.to_string()).unwrap_or("?".to_string()),
                    e.column.map(|n| n.to_string()).unwrap_or("?".to_string()),
                );
            }
        }
        None => {
            println!("No mapping found for offset {}", target_offset);
        }
    }
}
