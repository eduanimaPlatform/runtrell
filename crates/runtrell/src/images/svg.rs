// SPDX-License-Identifier: Apache-2.0
//! XML transport validation; never rewrites source or authorizes browser publication.
use super::{Result, invalid};
use quick_xml::{events::Event, name::ResolveResult, reader::NsReader};

pub(super) fn inspect(xml: &str) -> Result<()> {
    let bad = || invalid("SVG_XML");
    let mut reader = NsReader::from_str(xml);
    reader.config_mut().check_comments = true;
    let mut depth = 0usize;
    let mut root = false;
    loop {
        let (ns, event) = reader.read_resolved_event().map_err(|_| bad())?;
        match event {
            Event::Start(ref tag) | Event::Empty(ref tag) => {
                if matches!(ns, ResolveResult::Unknown(_)) {
                    return Err(bad());
                }
                if depth == 0 {
                    if root
                        || tag.local_name().as_ref() != "svg"
                        || !matches!(ns, ResolveResult::Bound(n) if n.as_ref() == "http://www.w3.org/2000/svg")
                    {
                        return Err(bad());
                    }
                    root = true;
                }
                for attr in tag.attributes() {
                    attr.map_err(|_| bad())?;
                }
                if matches!(event, Event::Start(_)) {
                    depth += 1;
                }
            }
            Event::End(_) => {
                depth = depth.checked_sub(1).ok_or_else(bad)?;
            }
            Event::Text(t) if depth == 0 => {
                if !t.as_ref().as_bytes().iter().all(u8::is_ascii_whitespace) {
                    return Err(bad());
                }
            }
            Event::CData(_) | Event::GeneralRef(_) if depth == 0 => return Err(bad()),
            Event::DocType(_) => return Err(bad()),
            Event::Eof => {
                return if root && depth == 0 {
                    Ok(())
                } else {
                    Err(bad())
                };
            }
            _ => (),
        }
    }
}
