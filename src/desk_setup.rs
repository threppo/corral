//! Saved desk setups. Independent policy from desk-setups-spec sections 2-5.
//! OS reads live in `windows`; all pairing, restore and learning below are pure.
#![allow(dead_code)]
use crate::{geometry::{Monitor, Rect}, model::{Layout, LoosePosition, RollState}};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::{BTreeMap, BTreeSet}, path::{Path, PathBuf}, io};
mod windows;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeHardware { pub manufacturer:String, pub product:String, pub serial:String, pub friendly_name:String, pub instance:String }
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model { pub mfr: String, pub product:u16 }
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connector { pub tech:u32, pub instance:u32 }
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorIdentity { pub model:Option<Model>, pub serial_hash:Option<String>, pub connector:Option<Connector>, pub name:Option<String> }
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorState {
    #[serde(flatten)] pub id:MonitorIdentity,
    pub size_px:(u32,u32), pub scale_pct:u32, pub offset_px:(i32,i32), pub primary:bool,
    #[serde(skip)] pub gdi_name:String,
    #[serde(skip)] pub work:Rect,
    #[serde(skip_serializing_if="Option::is_none")] pub legacy_gdi_name:Option<String>,
    /// Work area stored by an earlier version; kept verbatim, never matched (spec 5.2).
    #[serde(skip_serializing_if="Option::is_none")] pub legacy_work:Option<Rect>,
    #[serde(flatten)] pub extra:BTreeMap<String,Value>,
}
impl MonitorState {
    pub fn label(&self, n:usize)->String { self.id.name.clone().filter(|s|!s.is_empty()).unwrap_or_else(|| {
        if self.id.connector.as_ref().is_some_and(|c| c.tech==0x80000000 || c.tech==11 || c.tech==13) {"Built-in display".into()}
        else {format!("Display {} ({}x{})",n+1,self.size_px.0,self.size_px.1)}
    }) }
    fn rank(&self)->(i32,i32) { self.offset_px }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SetupPlacement {
    pub monitor:Option<usize>, pub rect:Rect, pub roll:RollState, pub dock_top:bool, pub dock_bottom:bool,
    #[serde(skip_serializing_if="Option::is_none")] pub preferred_size_dip:Option<(i32,i32)>,
    #[serde(flatten)] pub extra:BTreeMap<String,Value>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeskSetup {
    pub id:String, pub name:String, pub origin:String, pub last_used_ms:i64, pub modified_ms:i64,
    pub monitors:Vec<MonitorState>, pub groups:BTreeMap<u64,SetupPlacement>, pub loose:BTreeMap<String,LoosePosition>,
    #[serde(skip_serializing_if="Option::is_none")] pub legacy_key:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub legacy:Option<Value>,
    #[serde(flatten)] pub extra:BTreeMap<String,Value>,
}
impl DeskSetup { pub fn archived(&self)->bool { self.legacy.as_ref().and_then(|l|l.get("obsolete")).and_then(Value::as_bool)==Some(true) } }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeskSetups {
    pub version:u32, pub active:Option<String>, pub next_id:u64, pub setups:Vec<DeskSetup>,
    /// `LayoutPreserve` as the engine last saw it; a later start with 2 adopts the live layout instead of restoring.
    #[serde(skip_serializing_if="Option::is_none")] pub mode:Option<i64>,
    #[serde(skip_serializing_if="BTreeMap::is_empty")] pub legacy_unparsed:BTreeMap<String,Value>,
    #[serde(flatten)] pub extra:BTreeMap<String,Value>,
    #[serde(skip)] pub runtime:Runtime,
}
impl Default for DeskSetups { fn default()->Self { Self{version:1,active:None,next_id:1,setups:vec![],mode:None,legacy_unparsed:BTreeMap::new(),extra:BTreeMap::new(),runtime:Runtime::default()} } }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Runtime {
    pub current:Vec<MonitorState>, pub decision:Decision,
    pub baseline:Option<(BTreeMap<u64,SetupPlacement>, BTreeMap<String,LoosePosition>)>,
    pub status_text:String,
    pub edit_pending:bool,
    /// True while a drag is cancelled because the displays changed: that move is not a user edit (spec 3.2).
    pub display_cancel:bool,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Case { Exact, Adjusted, Missing, New, #[default] Unknown }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decision { pub case:Case, pub base:Option<usize>, pub pairs:Vec<(usize,usize)>, pub positional:bool }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request { pub nonce:u64, pub layout_path:String, pub action:Action }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag="type",rename_all="snake_case")]
pub enum Action { Forget{id:String}, SwapMonitors }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestResult { pub nonce:u64, pub ok:bool, pub message:String }

impl DeskSetups {
    pub fn visible(&self)->Vec<&DeskSetup> { let mut s:Vec<_>=self.setups.iter().filter(|s|!s.archived()).collect(); s.sort_by_key(|s|(std::cmp::Reverse(s.last_used_ms),s.id.clone())); s }
    pub fn forget(&mut self,id:&str)->bool { if self.active.as_deref()==Some(id) {return false} let n=self.setups.len();self.setups.retain(|s|s.id!=id);n!=self.setups.len() }
    pub fn status(&self)->String { if self.runtime.status_text.is_empty() {"New monitor setup. Move a Corral to save a layout for it.".into()}else{self.runtime.status_text.clone()} }
}

/// desk-setups-spec 2.2: discard duplicated/fake device serials before pairing.
pub fn normalize(mut monitors:Vec<MonitorState>)->Vec<MonitorState> {
    let duplicates:Vec<_>=monitors.iter().enumerate().filter(|(i,m)|m.id.serial_hash.is_some() && monitors.iter().enumerate().any(|(j,n)|i!=&j && m.id.model==n.id.model && m.id.serial_hash==n.id.serial_hash)).map(|(i,_)|i).collect();
    for i in duplicates {monitors[i].id.serial_hash=None;}
    monitors
}
pub fn fallback_monitors(monitors:&[Monitor])->Vec<MonitorState> {
    let x=monitors.iter().map(|m|m.bounds.left).min().unwrap_or(0);let y=monitors.iter().map(|m|m.bounds.top).min().unwrap_or(0);
    monitors.iter().map(|m|MonitorState {size_px:(m.bounds.width().max(0) as u32,m.bounds.height().max(0) as u32),scale_pct:((u64::from(m.dpi)*100+48)/96) as u32,offset_px:(m.bounds.left.saturating_sub(x),m.bounds.top.saturating_sub(y)),primary:m.bounds.left==0&&m.bounds.top==0,gdi_name:m.name.clone(),work:m.work,..Default::default()}).collect()
}
pub fn read_monitors(monitors:&[Monitor])->Vec<MonitorState> {normalize(windows::read_monitors(monitors))}
fn serial_ok(a:&MonitorState,b:&MonitorState)->bool { !matches!((&a.id.serial_hash,&b.id.serial_hash),(Some(x),Some(y)) if x!=y) }
fn same_model(a:&MonitorState,b:&MonitorState)->bool {a.id.model.is_some()&&a.id.model==b.id.model}
/// desk-setups-spec 4.1. Each pass removes paired monitors; arrangement rank is deterministic.
pub fn pair(saved:&[MonitorState],current:&[MonitorState])->(Vec<(usize,usize)>,bool) {
    let saved=normalize(saved.to_vec()); let current=normalize(current.to_vec());
    let mut pairs=vec![]; let mut used_s=BTreeSet::new();let mut used_c=BTreeSet::new();let mut positional=false;
    for pass in 0..4 {
        let mut cs:Vec<_>=(0..current.len()).filter(|c|!used_c.contains(c)).collect();cs.sort_by_key(|&c|(current[c].rank(),c));
        for c in cs {
            let a=&current[c];
            let compatible=|b:&MonitorState| match pass {
                0=>same_model(a,b)&&a.id.serial_hash.is_some()&&a.id.serial_hash==b.id.serial_hash,
                1=>same_model(a,b)&&serial_ok(a,b)&&a.id.connector.is_some()&&a.id.connector==b.id.connector,
                2=>same_model(a,b)&&serial_ok(a,b),
                _=>(a.id.model.is_none()||b.id.model.is_none())&&a.size_px==b.size_px,
            };
            let mut ss:Vec<_>=(0..saved.len()).filter(|s|!used_s.contains(s)&&compatible(&saved[*s])).collect();ss.sort_by_key(|&s|(saved[s].rank(),s));
            if pass==1 && (ss.len()!=1 || current.iter().enumerate().filter(|(k,n)|!used_c.contains(k)&&same_model(n,&saved[ss[0]])&&serial_ok(n,&saved[ss[0]])&&n.id.connector==saved[ss[0]].id.connector).count()!=1) {continue}
            if let Some(&s)=ss.first() {
                if pass==2 && (ss.len()>1 || saved.iter().filter(|n|same_model(n,a)).count()>1 || current.iter().filter(|n|same_model(n,a)).count()>1) {positional=true;}
                pairs.push((s,c));used_s.insert(s);used_c.insert(c);
            }
        }
    }
    (pairs,positional)
}
/// Legacy wire-key codec only, retained for desk-setups-spec 5.2 migration.
pub fn legacy_signature(monitors:&[Monitor])->String {
    let mut ms=monitors.to_vec();ms.sort_by_key(|m|(!(m.bounds.left==0&&m.bounds.top==0),std::cmp::Reverse(i64::from(m.bounds.width())*i64::from(m.bounds.height())),std::cmp::Reverse(m.bounds.width()),std::cmp::Reverse(m.dpi)));
    let mut key=format!("{}:",ms.len());for m in ms {key.push_str(&format!("[{}x{}@{}]{}",m.bounds.width(),m.bounds.height(),u64::from(m.dpi)*100/96,if m.bounds.left==0&&m.bounds.top==0{"+"}else{""}));}key
}
fn current_signature(current:&[MonitorState])->String {
    let min=current.iter().find(|m|m.primary).map(|m|m.offset_px).unwrap_or((0,0));
    let ms:Vec<_>=current.iter().map(|m|Monitor{name:m.gdi_name.clone(),bounds:Rect::new(m.offset_px.0-min.0,m.offset_px.1-min.1,(i64::from(m.offset_px.0)-i64::from(min.0)+i64::from(m.size_px.0)) as i32,(i64::from(m.offset_px.1)-i64::from(min.1)+i64::from(m.size_px.1)) as i32),work:m.work,dpi:(m.scale_pct*96+50)/100}).collect();legacy_signature(&ms)
}
pub fn decide(current:&[MonitorState],setups:&[DeskSetup],_active:Option<&str>)->Decision {
    if current.is_empty() {return Decision::default()}
    let mut candidates=vec![];
    for (i,s) in setups.iter().enumerate().filter(|(_,s)|!s.archived()&&!s.monitors.is_empty()) {
        let (pairs,positional)=pair(&s.monitors,current);if pairs.is_empty(){continue}
        let all=pairs.len()==current.len()&&pairs.len()==s.monitors.len();
        let geometry=all&&pairs.iter().all(|&(a,b)| {let a=&s.monitors[a];let b=&current[b];a.size_px==b.size_px&&a.scale_pct==b.scale_pct&&a.offset_px==b.offset_px});
        let exact=if s.origin=="legacy" {all&&s.legacy_key.as_deref()==Some(current_signature(current).as_str())}else{geometry};
        let new=current.len()-pairs.len();let missing=s.monitors.len()-pairs.len();
        let case=if exact{Case::Exact}else if missing>0{Case::Missing}else if new>0{Case::New}else{Case::Adjusted};
        candidates.push(((!exact,new,missing,std::cmp::Reverse(s.last_used_ms),i),Decision{case,base:Some(i),pairs,positional}));
    }
    candidates.sort_by_key(|x|x.0);candidates.into_iter().next().map(|(_,d)|d).unwrap_or_default()
}
/// desk-setups-spec 4.3: distance in saved arrangement, ties by arrangement rank.
pub fn recovery_target(home:usize,saved:&[MonitorState],current:&[MonitorState],pairs:&[(usize,usize)])->Option<usize> {
    let h=saved.get(home)?;let centre=|m:&MonitorState|(i128::from(m.offset_px.0)*2+i128::from(m.size_px.0),i128::from(m.offset_px.1)*2+i128::from(m.size_px.1));let (hx,hy)=centre(h);
    if let Some((_,c))=pairs.iter().min_by_key(|(s,c)|{let m=&saved[*s];let(x,y)=centre(m);((hx-x).pow(2)+(hy-y).pow(2),m.rank(),*c)}) {return Some(*c)}
    current.iter().enumerate().min_by_key(|(c,m)|{let(x,y)=centre(m);((hx-x).pow(2)+(hy-y).pow(2),m.rank(),*c)}).map(|(c,_)|c)
}
fn status(bank:&DeskSetups,d:&Decision,current:&[MonitorState])->String {
    let base=d.base.and_then(|i|bank.setups.get(i));
    let mut text=match d.case {
        Case::Exact=>"Saved layout restored.".into(),
        Case::Adjusted=>"Your monitor settings changed. Corrals are fitted for now; move one to save a layout for these settings.".into(),
        Case::Missing=>{let b=base.unwrap();let h=(0..b.monitors.len()).find(|s|!d.pairs.iter().any(|(a,_)|a==s)).unwrap();let c=recovery_target(h,&b.monitors,current,&d.pairs).unwrap_or(0);format!("{} is not connected. Its Corrals are shown on {} for now and go back when it returns.",b.monitors[h].label(h),current[c].label(c))},
        Case::New=>{let c=(0..current.len()).find(|c|!d.pairs.iter().any(|(_,b)|b==c)).unwrap();format!("{} is new. Move a Corral onto it to save a layout.",current[c].label(c))},
        Case::Unknown=>"New monitor setup. Move a Corral to save a layout for it.".into(),
    };if d.positional {text.push_str(" Identical monitors are matched by position. Use Swap monitors if they are the wrong way round.");}text
}
fn capture(layout:&Layout,current:&[MonitorState])->(BTreeMap<u64,SetupPlacement>,BTreeMap<String,LoosePosition>) {
    (layout.groups.iter().map(|g|(g.id,SetupPlacement{monitor:current.iter().position(|m|m.gdi_name.eq_ignore_ascii_case(&g.monitor)),rect:g.rect,roll:g.roll,dock_top:g.dock_top,dock_bottom:g.dock_bottom,preferred_size_dip:g.preferred_size_dip,..Default::default()})).collect(),layout.loose.clone())
}
/// Record the displayed baseline after fitting; this operation never writes setup data (spec 1).
pub fn acknowledge_display(layout:&mut Layout) {layout.desk_setups.runtime.baseline=Some(capture(layout,&layout.desk_setups.runtime.current));}
/// Evaluation restores only geometry, preserving live group membership/styles (spec 4.3).
pub fn evaluate(layout:&mut Layout,current:&[MonitorState],now:i64)->Decision {
    migrate(layout);
    let d=decide(current,&layout.desk_setups.setups,layout.desk_setups.active.as_deref());
    if let Some(i)=d.base {
        let base=layout.desk_setups.setups[i].clone();
        if d.case!=Case::New {
            let primary=current.iter().position(|m|m.primary).unwrap_or(0);
            for g in &mut layout.groups {
                if let Some(p)=base.groups.get(&g.id) {
                    let dest=p.monitor.and_then(|s|d.pairs.iter().find(|(a,_)|*a==s).map(|(_,c)|*c).or_else(||recovery_target(s,&base.monitors,current,&d.pairs))).unwrap_or(primary);
                    if let Some(m)=current.get(dest) {g.monitor=m.gdi_name.clone();g.rect=p.rect;g.roll=p.roll;g.dock_top=p.dock_top;g.dock_bottom=p.dock_bottom;g.preferred_size_dip=p.preferred_size_dip.or(Some((p.rect.width(),p.rect.height())));}
                }
            }
            // Legacy loose geometry is opaque; the lead permits restoring it only in Exact.
            if d.case==Case::Exact || (d.case==Case::Adjusted&&base.origin!="legacy") {layout.loose=base.loose.clone();}
        }
        layout.desk_setups.active=Some(base.id.clone());
        if d.case==Case::Exact {
            // Recency is updated on activation only, so repeated identical evaluations are inert.
            let activation=layout.desk_setups.runtime.decision.base!=Some(i)||layout.desk_setups.runtime.decision.case!=Case::Exact;
            if activation {layout.desk_setups.setups[i].last_used_ms=now;}
            upgrade(&mut layout.desk_setups.setups[i],current,&d);
        }
    } else {layout.desk_setups.active=None;}
    layout.desk_setups.runtime.status_text=status(&layout.desk_setups,&d,current);
    layout.desk_setups.runtime.current=current.to_vec();layout.desk_setups.runtime.decision=d.clone();acknowledge_display(layout);d
}
fn upgrade(s:&mut DeskSetup,current:&[MonitorState],d:&Decision) {
    if d.case!=Case::Exact {return}
    // Identity follows the pairing that made the match Exact (saved index -> current), not GDI names (spec 5.2).
    if d.pairs.len()!=s.monitors.len() {return}
    let mut ms=s.monitors.clone();
    for &(si,ci) in &d.pairs {
        let (Some(c),Some(m))=(current.get(ci),ms.get_mut(si))else{return};
        if m.id.model.is_some(){continue}
        if m.size_px!=c.size_px {return}
        m.id=c.id.clone();m.legacy_gdi_name=Some(c.gdi_name.clone());
    }
    s.monitors=ms;s.origin="learned".into();
}
/// Explicit user edit only; never call for fit, hover, restore or monitor notifications (spec 3.2).
pub fn user_edit(layout:&mut Layout,now:i64,settling:bool)->bool {
    if settling||layout.desk_setups.runtime.current.is_empty(){return false}
    let current=layout.desk_setups.runtime.current.clone();let content=capture(layout,&current);
    if layout.desk_setups.runtime.baseline.as_ref()==Some(&content){return false}
    let exact=layout.desk_setups.runtime.decision.case==Case::Exact;
    let target=if exact {layout.desk_setups.active.as_ref().and_then(|id|layout.desk_setups.setups.iter().position(|s|&s.id==id))}else{None};
    let mut groups=content.0;
    if let Some(i)=target {
        // Placements index the saved monitor list, which pairing may order differently from the live list (spec 3.1).
        let pairs=layout.desk_setups.runtime.decision.pairs.clone();
        for p in groups.values_mut() {if let Some(c)=p.monitor {p.monitor=pairs.iter().find(|(_,k)|*k==c).map(|(s,_)|*s).or(Some(c));}}
        let s=&layout.desk_setups.setups[i];
        if s.groups==groups && s.loose==content.1 {acknowledge_display(layout);return false}
    }
    let index=if let Some(i)=target {i}else{
        let bank=&mut layout.desk_setups;
        while bank.setups.len()>=16 {let victim=bank.setups.iter().enumerate().filter(|(_,s)|bank.active.as_deref()!=Some(&s.id)).min_by_key(|(i,s)|(s.last_used_ms,*i)).map(|(i,_)|i);if let Some(i)=victim{bank.setups.remove(i);}else{break}}
        let mut id=format!("s{}",bank.next_id);while bank.setups.iter().any(|s|s.id==id){bank.next_id+=1;id=format!("s{}",bank.next_id)}bank.next_id+=1;
        bank.setups.push(DeskSetup{id,name:current.iter().enumerate().map(|(i,m)|m.label(i)).collect::<Vec<_>>().join(" + "),origin:"learned".into(),monitors:current.iter().cloned().map(|mut m|{if m.id.model.is_none(){m.legacy_gdi_name=Some(m.gdi_name.clone());}m}).collect(),..Default::default()});bank.setups.len()-1
    };
    let s=&mut layout.desk_setups.setups[index];
    // Unknown extension fields on surviving placements are retained (spec 5.1).
    for (id,p) in &mut groups {if let Some(old)=s.groups.get(id){p.extra=old.extra.clone();}}
    s.groups=groups;s.loose=content.1;s.modified_ms=now;s.last_used_ms=now;
    layout.desk_setups.active=Some(s.id.clone());layout.desk_setups.runtime.status_text=format!("Layout saved for {}.",s.name);
    layout.desk_setups.runtime.decision=if target.is_some() {layout.desk_setups.runtime.decision.clone()}else{Decision{case:Case::Exact,base:Some(index),pairs:(0..current.len()).map(|n|(n,n)).collect(),positional:false}};acknowledge_display(layout);true
}

/// Migration reads raw JSON so bad entries cannot discard the rest of a layout (spec 5.2-5.3).
pub fn migrate(layout:&mut Layout) {
    if layout.profiles.is_null() {return}
    let raw=std::mem::take(&mut layout.profiles);let old_active=std::mem::take(&mut layout.active_profile);
    let bank=&mut layout.desk_setups;
    let Some(entries)=raw.as_object() else {bank.legacy_unparsed.insert("profiles".into(),raw);return};
    for (key,v) in entries {
        let Some(mut s)=migrate_entry(key,v)else{bank.legacy_unparsed.insert(key.clone(),v.clone());continue};
        s.id=format!("s{}",bank.next_id);bank.next_id+=1;
        if key==&old_active{bank.active=Some(s.id.clone());}bank.setups.push(s);
    }
    // Drop invalid downsizing references without using them for matching (spec 5.2).
    for s in &mut bank.setups {
        let Some(key)=s.legacy_key.as_deref()else{continue};let mut visited=BTreeSet::from([key.to_owned()]);let mut next=entries.get(key).and_then(|v|v.get("downsized_to")).and_then(Value::as_str);let mut bad=false;
        while let Some(n)=next {if !visited.insert(n.into())||!entries.contains_key(n){bad=true;break}next=entries.get(n).and_then(|v|v.get("downsized_to")).and_then(Value::as_str);}
        if bad {if let Some(l)=s.legacy.as_mut().and_then(Value::as_object_mut){l.remove("downsized_to");}eprintln!("corral: ignored invalid legacy downsized_to for {key}");}
    }
}
fn migrate_entry(key:&str,v:&Value)->Option<DeskSetup> {
    let obj=v.as_object()?;let ds=obj.get("displays")?.as_array()?;if ds.is_empty(){return None}
    #[derive(Deserialize)] struct OldDisplay{device:String,bounds:Rect,dpi:u32,#[serde(default)]work:Option<Rect>}
    let displays:Vec<OldDisplay>=ds.iter().map(|d|serde_json::from_value(d.clone()).ok()).collect::<Option<_>>()?;
    if displays.iter().any(|d|i64::from(d.bounds.right)-i64::from(d.bounds.left)<=0||i64::from(d.bounds.bottom)-i64::from(d.bounds.top)<=0||d.dpi==0){return None}
    let minx=displays.iter().map(|d|d.bounds.left).min()?;let miny=displays.iter().map(|d|d.bounds.top).min()?;
    let monitors:Vec<_>=displays.iter().map(|d|MonitorState{size_px:((i64::from(d.bounds.right)-i64::from(d.bounds.left))as u32,(i64::from(d.bounds.bottom)-i64::from(d.bounds.top))as u32),scale_pct:((u64::from(d.dpi)*100+48)/96) as u32,offset_px:(d.bounds.left.saturating_sub(minx),d.bounds.top.saturating_sub(miny)),primary:d.bounds.left==0&&d.bounds.top==0,legacy_gdi_name:Some(d.device.clone()),legacy_work:d.work,..Default::default()}).collect();
    let mut groups=BTreeMap::new();
    if let Some(gs)=obj.get("groups") {for (id,p) in gs.as_object()? {
        let id=id.parse().ok()?;let mut p=p.clone();let map=p.as_object_mut()?;let device=map.remove("monitor")?.as_str()?.to_owned();map.insert("monitor".into(),displays.iter().position(|d|d.device.eq_ignore_ascii_case(&device)).map(Value::from).unwrap_or(Value::Null));groups.insert(id,serde_json::from_value(p).ok()?);
    }}
    let loose=serde_json::from_value(obj.get("loose").cloned().unwrap_or_else(||serde_json::json!({}))).ok()?;
    let legacy=serde_json::json!({"established":obj.get("established").cloned().unwrap_or(Value::Null),"obsolete":obj.get("obsolete").cloned().unwrap_or(Value::Null),"downsized_to":obj.get("downsized_to").cloned().unwrap_or(Value::Null)});
    let mut extra=obj.clone();for k in ["displays","groups","loose","established","obsolete","downsized_to","last_used_ms","modified_ms"]{extra.remove(k);}
    Some(DeskSetup{name:format!("Earlier layout: {} monitors, {}",monitors.len(),monitors.iter().map(|m|format!("{}x{}",m.size_px.0,m.size_px.1)).collect::<Vec<_>>().join(" + ")),origin:"legacy".into(),monitors,groups,loose,legacy_key:Some(key.into()),legacy:Some(legacy),last_used_ms:obj.get("last_used_ms").map(|n|n.as_i64()).unwrap_or(Some(0))?,modified_ms:obj.get("modified_ms").map(|n|n.as_i64()).unwrap_or(Some(0))?,extra:extra.into_iter().collect(),..Default::default()})
}
pub fn migration_backup_path(path:&Path)->PathBuf {let mut p=path.as_os_str().to_owned();p.push(".before-desk-setups.bak");PathBuf::from(p)}
fn has_legacy_keys(bytes:&[u8])->bool {
    serde_json::from_slice::<Value>(bytes.strip_prefix(&[239,187,191]).unwrap_or(bytes)).is_ok_and(|v|v.get("profiles").is_some()||v.get("active_profile").is_some())
}
/// A failed copy prevents publication; create_new means a prior backup is never overwritten.
pub fn ensure_migration_backup(path:&Path)->io::Result<()> {
    let backup=migration_backup_path(path);
    if backup.is_file(){return Ok(())}
    // The legacy data comes from the primary file, or from `.bak` when the primary does not parse (load fell back to it).
    let source=[path.to_path_buf(),crate::model::backup_path(path)].into_iter().find_map(|p|std::fs::read(&p).ok().filter(|b|has_legacy_keys(b)));
    let Some(bytes)=source else {return Ok(())};
    if backup.exists(){return Err(io::Error::other("migration backup path is not a file"))}
    use std::io::Write;
    let mut f=match std::fs::OpenOptions::new().write(true).create_new(true).open(&backup){Ok(f)=>f,Err(e)if e.kind()==io::ErrorKind::AlreadyExists=>return Ok(()),Err(e)=>return Err(e)};
    if let Err(e)=f.write_all(&bytes).and_then(|_|f.sync_all()){drop(f);let _=std::fs::remove_file(&backup);return Err(e)}Ok(())
}
/// Adopt the live layout after Corral was not tracking setups (`LayoutPreserve` 0/1): treated as a user edit (spec 3.2).
pub fn adopt(layout:&mut Layout,current:&[MonitorState],now:i64)->bool {
    migrate(layout);
    let d=decide(current,&layout.desk_setups.setups,layout.desk_setups.active.as_deref());
    layout.desk_setups.active=d.base.map(|i|layout.desk_setups.setups[i].id.clone());
    layout.desk_setups.runtime.status_text=status(&layout.desk_setups,&d,current);
    layout.desk_setups.runtime.current=current.to_vec();layout.desk_setups.runtime.decision=d;
    if layout.groups.is_empty()&&layout.loose.is_empty() {acknowledge_display(layout);return false}
    layout.desk_setups.runtime.baseline=None;
    user_edit(layout,now,false)
}
/// A finished or cancelled drag is a user edit when it moved something, except a cancel caused by a display change.
pub fn records_drag_end(changed:bool,display_cancel:bool)->bool {changed&&!display_cancel}
/// Layout saves wait for the quiet period of a pending edit so the setup and layout are written once;
/// a crash inside those 500 ms loses that one edit.
pub fn defers_save(edit_pending:bool,settling:bool,enabled:bool)->bool {edit_pending&&!settling&&enabled}
/// Absolute, case-folded form of a layout path so the settings window and the engine compare the same string.
pub fn normalized_path(path:&Path)->String {
    let abs=std::path::absolute(path).unwrap_or_else(|_|path.to_path_buf());
    let text=abs.to_string_lossy().replace('/',"\\");
    text.trim_start_matches(r"\\?\").trim_end_matches('\\').to_lowercase()
}
#[derive(Serialize,Deserialize)]struct View {bank:DeskSetups,status:String}
fn view_path(path:&Path)->PathBuf{let mut s=path.as_os_str().to_owned();s.push(".desk-setups-view.json");PathBuf::from(s)}
pub fn publish_view(path:&Path,bank:&DeskSetups)->io::Result<()> {let bytes=serde_json::to_vec(&View{bank:bank.clone(),status:bank.status()}).map_err(io::Error::other)?;let p=view_path(path);if std::fs::read(&p).ok().as_deref()==Some(&bytes){return Ok(())}let mut t=p.as_os_str().to_owned();t.push(".tmp");let t=PathBuf::from(t);std::fs::write(&t,bytes)?;std::fs::rename(t,p)}
pub fn read_view(path:&Path)->Option<DeskSetups>{let mut v:View=serde_json::from_slice(&std::fs::read(view_path(path)).ok()?).ok()?;v.bank.runtime.status_text=v.status;Some(v.bank)}

#[cfg(test)] mod tests;
