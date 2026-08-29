use std::{collections::HashMap, env, fs, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::SystemTime};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct Library { connection: Arc<Mutex<Connection>>, media_dir: PathBuf, duration_cache: Arc<Mutex<HashMap<PathBuf, AudioDurationCacheEntry>>> }
#[derive(Debug, Clone)]
struct AudioDurationCacheEntry { len:u64, modified:Option<SystemTime>, seconds:f64 }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Song { pub id:String, pub title:String, pub audio_path:Option<String>, pub caption:String, pub lyrics:String, pub metadata:serde_json::Value, pub generation_settings:serde_json::Value, pub engine_id:String, pub profile_id:Option<String>, pub replay_request:Option<serde_json::Value>, pub audio_codes:Option<serde_json::Value>, pub source:String, pub created_at:String, pub updated_at:String }
#[derive(Debug, Clone, Deserialize)]
pub struct SongInput { pub title:String, pub audio_path:Option<String>, #[serde(default)] pub caption:String, #[serde(default)] pub lyrics:String, #[serde(default)] pub metadata:serde_json::Value, #[serde(default)] pub generation_settings:serde_json::Value, pub engine_id:String, pub profile_id:Option<String>, pub replay_request:Option<serde_json::Value>, pub audio_codes:Option<serde_json::Value>, #[serde(default="manual_source")] pub source:String }
#[derive(Debug, Clone)]
pub struct GeneratedSongInput { pub title:Option<String>, pub metadata:serde_json::Value, pub caption:String, pub lyrics:String, pub generation_settings:serde_json::Value, pub replay_request:Option<serde_json::Value>, pub audio_codes:Option<serde_json::Value>, pub engine_id:String, pub profile_id:Option<String>, pub source:String, pub audio_extension:&'static str, pub audio:Vec<u8> }
#[derive(Debug, Clone)]
pub struct AudioImportInput { pub title:String, pub caption:String, pub lyrics:String, pub metadata:serde_json::Value, pub generation_settings:serde_json::Value, pub engine_id:String, pub profile_id:Option<String>, pub source:String, pub audio_extension:String, pub audio:Vec<u8> }
#[derive(Debug, Clone, Serialize)]
pub struct ImportedSong { pub song: Song, pub audio_filename: String }
#[derive(Debug, Clone, Serialize, Deserialize)] pub struct Playlist { pub id:String, pub name:String, pub description:Option<String>, pub song_ids:Vec<String>, pub created_at:String, pub updated_at:String }
#[derive(Debug, Clone, Deserialize)] pub struct PlaylistInput { pub name:String, pub description:Option<String>, #[serde(default)] pub song_ids:Vec<String> }
fn manual_source()->String{"manual".into()}

/// Playable length measured from the audio itself.
///
/// The engine's replay request is sparse — a 60-second track omits the
/// `duration` field because 60 is the default — so the only dependable source
/// for a library row is the rendered file. WAV is read from its header; MP3 is
/// derived from its audio payload and the declared or first-frame bitrate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WavAudioEvidence {
 pub channels:u16,
 pub sample_rate_hz:u32,
 pub bits_per_sample:u16,
 pub data_bytes:u64,
 pub duration_seconds:f64,
}

/// Parses the bounded PCM WAV contract produced by the Compute Hub Music Worker.
///
/// RIFF chunks are walked by their declared sizes instead of searching raw audio
/// bytes for marker text. The result is suitable for a persisted receipt.
pub fn wav_audio_evidence(audio:&[u8])->Option<WavAudioEvidence>{
 if audio.len()<12||&audio[0..4]!=b"RIFF"||&audio[8..12]!=b"WAVE"{return None}
 let mut offset=12usize;
 let mut format=None;
 let mut data_bytes=None;
 while offset.checked_add(8)?<=audio.len(){
  let id=audio.get(offset..offset+4)?;
  let size=u32::from_le_bytes(audio.get(offset+4..offset+8)?.try_into().ok()?);
  let size=usize::try_from(size).ok()?;
  let chunk_start=offset.checked_add(8)?;
  let end=chunk_start.checked_add(size)?;
  if end>audio.len(){return None}
  if id==b"fmt "{
   if size<16{return None}
   let audio_format=u16::from_le_bytes(audio.get(chunk_start..chunk_start+2)?.try_into().ok()?);
   let channels=u16::from_le_bytes(audio.get(chunk_start+2..chunk_start+4)?.try_into().ok()?);
   let sample_rate_hz=u32::from_le_bytes(audio.get(chunk_start+4..chunk_start+8)?.try_into().ok()?);
   let byte_rate=u32::from_le_bytes(audio.get(chunk_start+8..chunk_start+12)?.try_into().ok()?);
   let block_align=u16::from_le_bytes(audio.get(chunk_start+12..chunk_start+14)?.try_into().ok()?);
   let bits_per_sample=u16::from_le_bytes(audio.get(chunk_start+14..chunk_start+16)?.try_into().ok()?);
   let expected_align=u32::from(channels).checked_mul(u32::from(bits_per_sample))?.checked_div(8)?;
   let expected_rate=sample_rate_hz.checked_mul(expected_align)?;
   if audio_format!=1||!(1..=2).contains(&channels)||!(8_000..=96_000).contains(&sample_rate_hz)
    ||![8,16,24,32].contains(&bits_per_sample)||u32::from(block_align)!=expected_align||byte_rate!=expected_rate{
    return None
   }
   format=Some((channels,sample_rate_hz,bits_per_sample,byte_rate,block_align));
  }else if id==b"data"{
   if size==0{return None}
   data_bytes=Some(size as u64);
  }
  offset=end.checked_add(size%2)?;
 }
 let (channels,sample_rate_hz,bits_per_sample,byte_rate,block_align)=format?;
 let data_bytes=data_bytes?;
 if data_bytes%u64::from(block_align)!=0{return None}
 Some(WavAudioEvidence{
  channels,sample_rate_hz,bits_per_sample,data_bytes,
  duration_seconds:data_bytes as f64/f64::from(byte_rate),
 })
}

pub fn audio_duration_seconds(audio:&[u8],extension:&str,declared_bitrate_kbps:Option<u32>)->Option<f64>{
 match extension{
  "wav"=>wav_audio_evidence(audio).map(|evidence|evidence.duration_seconds),
  "mp3"=>{
   let bitrate=declared_bitrate_kbps.or_else(||mp3_bitrate_kbps(audio))? as f64*1000.0;
   let payload=mp3_payload_bytes(audio)? as f64;
   (bitrate>0.0).then(||(payload*8.0)/bitrate)
  }
  _=>None,
 }
}

fn mp3_payload_bytes(audio:&[u8])->Option<usize>{
 let mut start=0usize;
 if audio.starts_with(b"ID3"){
  let size=audio.get(6..10)?;
  if size.iter().any(|byte|byte&0x80!=0){return None}
  let tag_size=((size[0] as usize)<<21)|((size[1] as usize)<<14)|((size[2] as usize)<<7)|(size[3] as usize);
  start=10usize.checked_add(tag_size)?;
  if audio.get(5).is_some_and(|flags|flags&0x10!=0){start=start.checked_add(10)?}
 }
 let mut end=audio.len();
 if end>=128&&audio.get(end-128..end-125)==Some(b"TAG"){end-=128}
 (end>start).then_some(end-start)
}

fn mp3_bitrate_kbps(audio:&[u8])->Option<u32>{
 let start=if audio.starts_with(b"ID3"){
  let size=audio.get(6..10)?;
  if size.iter().any(|byte|byte&0x80!=0){return None}
  10+(((size[0] as usize)<<21)|((size[1] as usize)<<14)|((size[2] as usize)<<7)|(size[3] as usize))
 }else{0};
 for frame in audio.get(start..)?.windows(4).take(65_536){
  let header=u32::from_be_bytes(frame.try_into().ok()?);
  if header&0xffe0_0000!=0xffe0_0000{continue}
  let version=(header>>19)&0b11;
  let layer=(header>>17)&0b11;
  let index=((header>>12)&0b1111) as usize;
  if version==0b01||layer!=0b01||index==0||index==15{continue}
  const MPEG1_LAYER3:[u32;16]=[0,32,40,48,56,64,80,96,112,128,160,192,224,256,320,0];
  const MPEG2_LAYER3:[u32;16]=[0,8,16,24,32,40,48,56,64,80,96,112,128,144,160,0];
  return Some(if version==0b11{MPEG1_LAYER3[index]}else{MPEG2_LAYER3[index]});
 }
 None
}

fn configured_bitrate_kbps(settings:&serde_json::Value)->Option<u32>{
 ["/payload/bitrate","/payload/mp3_bitrate","/bitrate","/mp3_bitrate"]
  .iter()
  .find_map(|pointer|settings.pointer(pointer).and_then(serde_json::Value::as_u64))
  .and_then(|value|{
   let value=if value>=8_000{(value+500)/1_000}else{value};
   u32::try_from(value).ok().filter(|value|*value>0)
  })
}

fn metadata_with_measured_duration(mut metadata:serde_json::Value,actual:f64)->serde_json::Value{
 let requested=metadata.get("duration_seconds").and_then(serde_json::Value::as_f64);
 if !metadata.is_object(){metadata=serde_json::json!({})}
 let fields=metadata.as_object_mut().expect("metadata was normalized to an object");
 if let Some(requested)=requested.filter(|requested|(requested-actual).abs()>0.25){
  if let Some(value)=serde_json::Number::from_f64(requested){fields.entry("requested_duration_seconds").or_insert(serde_json::Value::Number(value));}
 }
 if let Some(value)=serde_json::Number::from_f64(actual){
  fields.insert("actual_duration_seconds".into(),serde_json::Value::Number(value.clone()));
  fields.insert("duration_seconds".into(),serde_json::Value::Number(value));
  fields.insert("duration_source".into(),serde_json::Value::String("audio_file".into()));
 }
 metadata
}
fn metadata_with_actual_duration(metadata:serde_json::Value,audio:&[u8],extension:&str,settings:&serde_json::Value)->serde_json::Value{
 let Some(actual)=audio_duration_seconds(audio,extension,configured_bitrate_kbps(settings)) else{return metadata};
 metadata_with_measured_duration(metadata,actual)
}

/// A caption is a full style prompt, not a song name. Without an explicit
/// title the library shows a readable fragment instead of the whole prompt.
/// A caption written for this model is a labelled document, so its first line
/// is a section heading - "Global Metadata" - which makes a useless title. Skip
/// the headings and take the first line that actually describes the music.
const CAPTION_HEADINGS:[&str;5]=["global metadata","vocal details","arrangement","basic attributes","instrument lifecycle description"];
/// "bpm is 118" and "key is F# minor" describe the music but read terribly as a
/// name, so the title skips past them to the first descriptive phrase.
const CAPTION_ATTRIBUTES:[&str;5]=["bpm is","key is","scale is","tempo is","time signature"];
fn generated_title(caption:&str)->String{
 let first=caption.split(['\n','.',';']).map(str::trim)
  .find(|part|{
   let lowered=part.to_ascii_lowercase();
   !part.is_empty()
    && !CAPTION_HEADINGS.iter().any(|heading|lowered.starts_with(heading))
    && !CAPTION_ATTRIBUTES.iter().any(|attribute|lowered.contains(attribute))
  })
  .unwrap_or("Untitled track");
 let mut title=String::new();
 for word in first.split_whitespace(){
  if !title.is_empty() && title.chars().count()+1+word.chars().count()>48 {break}
  if !title.is_empty(){title.push(' ')}
  title.push_str(word);
 }
 if title.is_empty(){"Untitled track".into()}else{title}
}

/// What an image actually is, read off its first bytes.
///
/// Every picture format announces itself: a claim in a header or a filename is
/// a claim, this is the fact.
pub fn sniff_image_type(image:&[u8])->Option<&'static str>{
 if image.starts_with(&[0xFF,0xD8,0xFF]){return Some("image/jpeg")}
 if image.starts_with(&[0x89,b'P',b'N',b'G',0x0D,0x0A,0x1A,0x0A]){return Some("image/png")}
 if image.len()>=12&&&image[..4]==b"RIFF"&&&image[8..12]==b"WEBP"{return Some("image/webp")}
 None
}

#[cfg(test)]
mod image_type_tests {
    #[test]
    fn every_format_is_recognised_by_its_own_bytes() {
        assert_eq!(super::sniff_image_type(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(super::sniff_image_type(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]), Some("image/png"));
        let mut webp = b"RIFF0000WEBP".to_vec();
        webp.extend_from_slice(b"VP8 ");
        assert_eq!(super::sniff_image_type(&webp), Some("image/webp"));
        assert_eq!(super::sniff_image_type(b"not a picture"), None);
    }
}

impl Library {
 /// The library lives in the same place as models and settings. Resolving it
    /// separately used to put the database under `<cwd>/data` whenever the
    /// service was started outside the desktop shell, so the same install
    /// showed two different libraries depending on how it was launched.
    pub fn open_default()->Result<Self>{let root=crate::studio_data_root().unwrap_or_else(||env::current_dir().unwrap_or_else(|_|PathBuf::from(".")).join("data"));Self::open_at(root.join("library.sqlite"),root.join("media"))}
 pub fn open_at(db_path:PathBuf,media_dir:PathBuf)->Result<Self>{if let Some(p)=db_path.parent(){fs::create_dir_all(p)?};let c=Connection::open(db_path)?;c.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE IF NOT EXISTS songs(id TEXT PRIMARY KEY,title TEXT NOT NULL,audio_path TEXT,caption TEXT NOT NULL,lyrics TEXT NOT NULL,metadata_json TEXT NOT NULL,generation_settings_json TEXT NOT NULL,engine_id TEXT NOT NULL,profile_id TEXT,replay_request_json TEXT,audio_codes_json TEXT,source TEXT NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL); CREATE TABLE IF NOT EXISTS playlists(id TEXT PRIMARY KEY,name TEXT NOT NULL,description TEXT,created_at TEXT NOT NULL,updated_at TEXT NOT NULL); CREATE TABLE IF NOT EXISTS playlist_songs(playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,song_id TEXT NOT NULL REFERENCES songs(id) ON DELETE CASCADE,position INTEGER NOT NULL,PRIMARY KEY(playlist_id,song_id));")?;Ok(Self{connection:Arc::new(Mutex::new(c)),media_dir,duration_cache:Arc::new(Mutex::new(HashMap::new()))})}
 pub fn list_songs(&self)->Result<Vec<Song>>{let c=self.connection.lock().unwrap();let mut s=c.prepare("SELECT id,title,audio_path,caption,lyrics,metadata_json,generation_settings_json,engine_id,profile_id,replay_request_json,audio_codes_json,source,created_at,updated_at FROM songs ORDER BY created_at DESC")?;let songs=s.query_map([],row_song)?.collect::<rusqlite::Result<Vec<_>>>()?;drop(s);drop(c);Ok(songs.into_iter().map(|song|self.song_with_actual_duration(song)).collect())}
 pub fn get_song(&self,id:&str)->Result<Option<Song>>{let c=self.connection.lock().unwrap();let song=c.query_row("SELECT id,title,audio_path,caption,lyrics,metadata_json,generation_settings_json,engine_id,profile_id,replay_request_json,audio_codes_json,source,created_at,updated_at FROM songs WHERE id=?",[id],row_song).optional()?;drop(c);Ok(song.map(|song|self.song_with_actual_duration(song)))}
 fn song_with_actual_duration(&self,mut song:Song)->Song{
  if song.metadata.get("duration_source").and_then(serde_json::Value::as_str)==Some("audio_file")&&song.metadata.get("actual_duration_seconds").and_then(serde_json::Value::as_f64).is_some(){return song}
  let Some(path)=song.audio_path.as_deref().map(Path::new) else{return song};
  let Ok(file)=fs::metadata(path) else{return song};
  let len=file.len();let modified=file.modified().ok();let cache_key=path.to_path_buf();
  let cached=self.duration_cache.lock().ok().and_then(|cache|cache.get(&cache_key).filter(|entry|entry.len==len&&entry.modified==modified).cloned());
  let actual=cached.map(|entry|entry.seconds).or_else(||{
   let extension=path.extension()?.to_str()?.to_ascii_lowercase();let audio=fs::read(path).ok()?;
   let seconds=audio_duration_seconds(&audio,&extension,configured_bitrate_kbps(&song.generation_settings))?;
   if let Ok(mut cache)=self.duration_cache.lock(){cache.insert(cache_key,AudioDurationCacheEntry{len,modified,seconds});}
   Some(seconds)
  });
  if let Some(actual)=actual{song.metadata=metadata_with_measured_duration(song.metadata,actual)}
  song
 }
 pub fn create_song(&self,input:SongInput)->Result<Song>{let now=now();let song=Song{id:uuid::Uuid::now_v7().to_string(),title:input.title,audio_path:input.audio_path,caption:input.caption,lyrics:input.lyrics,metadata:input.metadata,generation_settings:input.generation_settings,engine_id:input.engine_id,profile_id:input.profile_id,replay_request:input.replay_request,audio_codes:input.audio_codes,source:input.source,created_at:now.clone(),updated_at:now};self.save_song(&song)?;Ok(song)}
 pub fn import_generated_song(&self,input:GeneratedSongInput)->Result<ImportedSong>{
  if input.audio.is_empty(){anyhow::bail!("cannot import an empty audio result")}
  fs::create_dir_all(&self.media_dir).with_context(||format!("create media directory {}",self.media_dir.display()))?;
  let id=uuid::Uuid::now_v7().to_string();let filename=format!("{id}.{}",input.audio_extension);let target=self.media_dir.join(&filename);let temporary=self.media_dir.join(format!("{filename}.part"));
  {let mut file=fs::OpenOptions::new().create_new(true).write(true).open(&temporary)?;use std::io::Write;file.write_all(&input.audio)?;file.sync_all()?;}
  fs::rename(&temporary,&target).with_context(||format!("publish generated audio {}",target.display()))?;
  let metadata=metadata_with_actual_duration(input.metadata,&input.audio,input.audio_extension,&input.generation_settings);
  let now=now();let title=input.title.map(|t|t.trim().to_owned()).filter(|t|!t.is_empty()).unwrap_or_else(||generated_title(&input.caption));let song=Song{id,title,audio_path:Some(target.display().to_string()),caption:input.caption,lyrics:input.lyrics,metadata,generation_settings:input.generation_settings,engine_id:input.engine_id,profile_id:input.profile_id,replay_request:input.replay_request,audio_codes:input.audio_codes,source:input.source,created_at:now.clone(),updated_at:now};
  if let Err(error)=self.save_song(&song){let _=fs::remove_file(&target);return Err(error.context("store generated song record"));}
  Ok(ImportedSong{song,audio_filename:filename})
 }
 /// Publishes a generated artifact under a caller-owned stable id. Retrying
 /// after a crash returns the same row/file and never creates a second song.
 pub fn import_generated_song_idempotent(&self,id:&str,input:GeneratedSongInput)->Result<ImportedSong>{
  if id.is_empty()||id.len()>200||!id.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'-'|b'_'|b'.')){anyhow::bail!("invalid stable generated song id")}
  if input.audio.is_empty(){anyhow::bail!("cannot import an empty audio result")}
  if let Some(existing)=self.get_song(id)?{
   let filename=existing.audio_path.as_deref().and_then(|p|Path::new(p).file_name()).and_then(|p|p.to_str()).unwrap_or_default().to_owned();
   if existing.source!="omnibridge_generation"||filename.is_empty(){anyhow::bail!("stable generated song id belongs to another source")}
   return Ok(ImportedSong{song:existing,audio_filename:filename});
  }
  fs::create_dir_all(&self.media_dir).with_context(||format!("create media directory {}",self.media_dir.display()))?;
  let extension=input.audio_extension.trim().to_ascii_lowercase();
  if !matches!(extension.as_str(),"mp3"|"wav"|"m4a"|"aac"|"flac"|"ogg"){anyhow::bail!("unsupported generated audio extension")}
  let filename=format!("{id}.{extension}");
  let target=self.media_dir.join(&filename);
  if target.exists(){
   let existing=fs::read(&target).with_context(||format!("read generated audio {}",target.display()))?;
   if existing!=input.audio{anyhow::bail!("stable generated audio path contains different bytes")}
  }else{
   let temporary=self.media_dir.join(format!("{filename}.part"));
   {let mut file=fs::OpenOptions::new().create_new(true).write(true).open(&temporary)?;use std::io::Write;file.write_all(&input.audio)?;file.sync_all()?;}
   if let Err(error)=fs::rename(&temporary,&target){let _=fs::remove_file(&temporary);return Err(error).with_context(||format!("publish generated audio {}",target.display()))}
  }
  let now=now();
  let title=input.title.map(|t|t.trim().to_owned()).filter(|t|!t.is_empty()).unwrap_or_else(||generated_title(&input.caption));
  let metadata=metadata_with_actual_duration(input.metadata,&input.audio,&extension,&input.generation_settings);
  let song=Song{
   id:id.to_owned(),title,audio_path:Some(target.display().to_string()),caption:input.caption,lyrics:input.lyrics,
   metadata,generation_settings:input.generation_settings,engine_id:input.engine_id,profile_id:input.profile_id,
   replay_request:input.replay_request,audio_codes:input.audio_codes,source:input.source,created_at:now.clone(),updated_at:now,
  };
  if let Err(error)=self.save_song(&song){return Err(error.context("store idempotent generated song record"))}
  Ok(ImportedSong{song,audio_filename:filename})
 }
 pub fn import_audio_song(&self,input:AudioImportInput)->Result<ImportedSong>{
  if input.audio.is_empty(){anyhow::bail!("cannot import an empty audio file")}
  let extension=input.audio_extension.trim().to_ascii_lowercase();
  if !matches!(extension.as_str(), "mp3" | "wav"){anyhow::bail!("only MP3 and WAV audio can be imported")}
  fs::create_dir_all(&self.media_dir).with_context(||format!("create media directory {}",self.media_dir.display()))?;
  let id=uuid::Uuid::now_v7().to_string(); let filename=format!("{id}.{extension}"); let target=self.media_dir.join(&filename); let temporary=self.media_dir.join(format!("{filename}.part"));
  {let mut file=fs::OpenOptions::new().create_new(true).write(true).open(&temporary)?;use std::io::Write;file.write_all(&input.audio)?;file.sync_all()?;}
  fs::rename(&temporary,&target).with_context(||format!("publish imported audio {}",target.display()))?;
  let now=now(); let song=Song{id,title:input.title,audio_path:Some(target.display().to_string()),caption:input.caption,lyrics:input.lyrics,metadata:input.metadata,generation_settings:input.generation_settings,engine_id:input.engine_id,profile_id:input.profile_id,replay_request:None,audio_codes:None,source:input.source,created_at:now.clone(),updated_at:now};
  if let Err(error)=self.save_song(&song){let _=fs::remove_file(&target);return Err(error.context("store imported song record"));}
  Ok(ImportedSong{song,audio_filename:filename})
 }
 /// Stores a cover image next to the track audio and records its filename in
 /// the song metadata. Covers are a Studio-side concept: the engine never sees
 /// them, so they are stored as plain media rather than in the request record.
 pub fn store_song_cover(&self,id:&str,image:&[u8],media_type:&str)->Result<Song>{
  if image.is_empty(){anyhow::bail!("cannot store an empty cover image")}
  // The declared type is a claim, the magic numbers are the fact. A model that
  // answered with a JPEG had it stored as `image/png`, and the tag inside the
  // mp3 said PNG over JPEG bytes - players that trust the tag showed nothing.
  let media_type=sniff_image_type(image).unwrap_or(media_type);
  let extension=match media_type.trim().to_ascii_lowercase().as_str(){
   "image/png"=>"png","image/jpeg"|"image/jpg"=>"jpg","image/webp"=>"webp",
   other=>anyhow::bail!("unsupported cover media type '{other}'; use PNG, JPEG or WebP"),
  };
  let Some(mut song)=self.get_song(id)? else{anyhow::bail!("song not found")};
  fs::create_dir_all(&self.media_dir).with_context(||format!("create media directory {}",self.media_dir.display()))?;
  let filename=format!("{id}-cover.{extension}");
  let target=self.media_dir.join(&filename);
  let temporary=self.media_dir.join(format!("{filename}.part"));
  {let mut file=fs::OpenOptions::new().create(true).write(true).truncate(true).open(&temporary)?;use std::io::Write;file.write_all(image)?;file.sync_all()?;}
  fs::rename(&temporary,&target).with_context(||format!("publish cover {}",target.display()))?;
  let previous=song.metadata.get("cover_filename").and_then(|v|v.as_str()).map(str::to_owned);
  if song.metadata.is_null(){song.metadata=serde_json::json!({})}
  if let Some(fields)=song.metadata.as_object_mut(){
   fields.insert("cover_filename".into(),serde_json::Value::String(filename.clone()));
   fields.insert("cover_media_type".into(),serde_json::Value::String(format!("image/{}",if extension=="jpg"{"jpeg"}else{extension})));
  }
  song.updated_at=now();
  if let Err(error)=self.save_song(&song){let _=fs::remove_file(&target);return Err(error.context("store song cover record"))}
  if let Some(previous)=previous.filter(|previous|previous!=&filename){let _=fs::remove_file(self.media_dir.join(previous));}
  Ok(song)
 }
 pub fn cover_path_for_song(&self,song:&Song)->Option<(PathBuf,String)>{
  let filename=song.metadata.get("cover_filename")?.as_str()?;
  let media_type=song.metadata.get("cover_media_type").and_then(|v|v.as_str()).unwrap_or("image/png").to_owned();
  let path=self.media_file(filename)?;
  // Covers stored before the type was read off the bytes carry the wrong one
  // in their record. The file itself still says what it is.
  let media_type=Self::read_head(&path).and_then(|head|sniff_image_type(&head)).map(str::to_owned).unwrap_or(media_type);
  Some((path,media_type))
 }
 fn read_head(path:&std::path::Path)->Option<Vec<u8>>{
  use std::io::Read;
  let mut file=fs::File::open(path).ok()?;
  let mut head=[0u8;16];
  let read=file.read(&mut head).ok()?;
  Some(head[..read].to_vec())
 }
 /// Where media for this library lives. Stems are written here so they
 /// sit beside the track they came from.
 pub fn media_dir(&self)->&Path{&self.media_dir}
 pub fn media_file(&self,filename:&str)->Option<PathBuf>{if filename.is_empty()||filename.contains(['/', '\\'])||Path::new(filename).file_name().and_then(|x|x.to_str())!=Some(filename){return None}let path=self.media_dir.join(filename);path.is_file().then_some(path)}
 pub fn media_path_for_song(&self,song:&Song)->Option<PathBuf>{let candidate=PathBuf::from(song.audio_path.as_ref()?);let root=self.media_dir.canonicalize().ok()?;let resolved=candidate.canonicalize().ok()?;resolved.starts_with(root).then_some(resolved)}
 /// Stores karaoke timings with the track. They live in the metadata rather
 /// than a column of their own so an existing library needs no migration, and
 /// a track without them is simply a track nobody has timed yet.
 pub fn set_song_lrc(&self,id:&str,lrc:&str)->Result<Option<Song>>{
  let Some(mut song)=self.get_song(id)? else{return Ok(None)};
  let mut metadata=match song.metadata.take(){serde_json::Value::Object(map)=>map,_=>serde_json::Map::new()};
  if lrc.trim().is_empty(){metadata.remove("lrc");}else{metadata.insert("lrc".into(),serde_json::Value::String(lrc.to_owned()));}
  song.metadata=serde_json::Value::Object(metadata);
  song.updated_at=now();
  self.save_song(&song)?;
  Ok(Some(song))
 }
 pub fn update_song(&self,id:&str,input:SongInput)->Result<Option<Song>>{let Some(mut song)=self.get_song(id)? else{return Ok(None)};song.title=input.title;if input.audio_path.is_some(){song.audio_path=input.audio_path};song.caption=input.caption;song.lyrics=input.lyrics;song.metadata=input.metadata;song.generation_settings=input.generation_settings;song.engine_id=input.engine_id;song.profile_id=input.profile_id;if input.replay_request.is_some(){song.replay_request=input.replay_request};if input.audio_codes.is_some(){song.audio_codes=input.audio_codes};song.source=input.source;song.updated_at=now();self.save_song(&song)?;Ok(Some(song))}
 pub fn delete_song(&self,id:&str)->Result<bool>{Ok(self.connection.lock().unwrap().execute("DELETE FROM songs WHERE id=?",[id])?>0)}
 /// Deletes the row and the media this library owns for it. External paths are
 /// never removed: every candidate must resolve through the managed media dir.
 pub fn delete_song_with_media(&self,id:&str)->Result<bool>{
  let Some(song)=self.get_song(id)? else{return Ok(false)};
  let mut owned=Vec::new();
  if let Some(path)=self.media_path_for_song(&song){owned.push(path)}
  if let Some((path,_))=self.cover_path_for_song(&song){owned.push(path)}
  for stem in crate::separation::STEMS{
   if let Some(path)=self.media_file(&format!("{id}-{stem}.wav")){owned.push(path)}
  }
  if !self.delete_song(id)?{return Ok(false)}
  owned.sort();owned.dedup();
  for path in owned{
   if let Err(error)=fs::remove_file(&path){
    if error.kind()!=std::io::ErrorKind::NotFound{eprintln!("could not remove deleted song media {}: {error}",path.display())}
   }
  }
  Ok(true)
 }
 fn save_song(&self,s:&Song)->Result<()> {self.connection.lock().unwrap().execute("INSERT INTO songs VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET title=excluded.title,audio_path=excluded.audio_path,caption=excluded.caption,lyrics=excluded.lyrics,metadata_json=excluded.metadata_json,generation_settings_json=excluded.generation_settings_json,engine_id=excluded.engine_id,profile_id=excluded.profile_id,replay_request_json=excluded.replay_request_json,audio_codes_json=excluded.audio_codes_json,source=excluded.source,updated_at=excluded.updated_at",params![s.id,s.title,s.audio_path,s.caption,s.lyrics,s.metadata.to_string(),s.generation_settings.to_string(),s.engine_id,s.profile_id,s.replay_request.as_ref().map(|v|v.to_string()),s.audio_codes.as_ref().map(|v|v.to_string()),s.source,s.created_at,s.updated_at])?;Ok(())}
 pub fn list_playlists(&self)->Result<Vec<Playlist>>{let c=self.connection.lock().unwrap();let mut s=c.prepare("SELECT id,name,description,created_at,updated_at FROM playlists ORDER BY created_at DESC")?;Ok(s.query_map([],|r|playlist(&c,r))?.collect::<rusqlite::Result<_>>()?)}
 pub fn get_playlist(&self,id:&str)->Result<Option<Playlist>>{let c=self.connection.lock().unwrap();c.query_row("SELECT id,name,description,created_at,updated_at FROM playlists WHERE id=?",[id],|r|playlist(&c,r)).optional().map_err(Into::into)}
 pub fn create_playlist(&self,input:PlaylistInput)->Result<Playlist>{let now=now();let p=Playlist{id:uuid::Uuid::now_v7().to_string(),name:input.name,description:input.description,song_ids:input.song_ids,created_at:now.clone(),updated_at:now};self.save_playlist(&p)?;Ok(p)}
 pub fn update_playlist(&self,id:&str,input:PlaylistInput)->Result<Option<Playlist>>{let Some(mut playlist)=self.get_playlist(id)? else{return Ok(None)};playlist.name=input.name;playlist.description=input.description;playlist.song_ids=input.song_ids;playlist.updated_at=now();self.save_playlist(&playlist)?;Ok(Some(playlist))}
 pub fn delete_playlist(&self,id:&str)->Result<bool>{Ok(self.connection.lock().unwrap().execute("DELETE FROM playlists WHERE id=?",[id])?>0)}
 fn save_playlist(&self,p:&Playlist)->Result<()> {let mut c=self.connection.lock().unwrap();let tx=c.transaction()?;tx.execute("INSERT INTO playlists VALUES(?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,description=excluded.description,updated_at=excluded.updated_at",params![p.id,p.name,p.description,p.created_at,p.updated_at])?;tx.execute("DELETE FROM playlist_songs WHERE playlist_id=?",[&p.id])?;for(pos,id)in p.song_ids.iter().enumerate(){tx.execute("INSERT INTO playlist_songs VALUES(?,?,?)",params![p.id,id,pos as i64])?;}tx.commit()?;Ok(())}
}
fn row_song(r:&rusqlite::Row)->rusqlite::Result<Song>{Ok(Song{id:r.get(0)?,title:r.get(1)?,audio_path:r.get(2)?,caption:r.get(3)?,lyrics:r.get(4)?,metadata:json(r.get::<_,String>(5)?),generation_settings:json(r.get::<_,String>(6)?),engine_id:r.get(7)?,profile_id:r.get(8)?,replay_request:r.get::<_,Option<String>>(9)?.map(json),audio_codes:r.get::<_,Option<String>>(10)?.map(json),source:r.get(11)?,created_at:r.get(12)?,updated_at:r.get(13)?})}fn json(s:String)->serde_json::Value{serde_json::from_str(&s).unwrap_or(serde_json::Value::Null)}fn playlist(c:&Connection,r:&rusqlite::Row)->rusqlite::Result<Playlist>{let id:String=r.get(0)?;let mut q=c.prepare("SELECT song_id FROM playlist_songs WHERE playlist_id=? ORDER BY position")?;let song_ids=q.query_map([&id],|x|x.get(0))?.collect::<rusqlite::Result<_>>()?;Ok(Playlist{id,name:r.get(1)?,description:r.get(2)?,song_ids,created_at:r.get(3)?,updated_at:r.get(4)?})}fn now()->String{std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs().to_string()}
#[cfg(test)]mod tests{use super::*;#[test]fn a_structured_caption_does_not_become_a_title_of_headings(){
  let caption="Global Metadata
Basic Attributes: bpm is 118. key is F# minor, and scale is minor. Darkwave, Synth-pop. Global Emotional Progression: haunting.";
  assert_eq!(generated_title(caption),"Darkwave, Synth-pop");
  assert_eq!(generated_title("Bright uplifting synth-pop, punchy drums"),"Bright uplifting synth-pop, punchy drums");
 }
#[test]fn imports_audio_and_full_provenance_atomically(){let root=std::env::temp_dir().join(format!("mm3-library-test-{}",uuid::Uuid::now_v7()));let db=Library::open_at(root.join("library.sqlite"),root.join("media")).unwrap();let result=db.import_generated_song(GeneratedSongInput{title:None,metadata:serde_json::Value::Null,caption:"c".into(),lyrics:"l".into(),generation_settings:serde_json::json!({"seed":1}),replay_request:Some(serde_json::json!({"audio_codes":"1,2,3,4,5,6,7,8","seed":1})),audio_codes:Some(serde_json::json!("1,2,3,4,5,6,7,8")),engine_id:"mm".into(),profile_id:Some("recommended-light".into()),source:"local_generation".into(),audio_extension:"mp3",audio:b"ID3".to_vec()}).unwrap();assert_eq!(db.get_song(&result.song.id).unwrap().unwrap().audio_codes,Some(serde_json::json!("1,2,3,4,5,6,7,8")));assert_eq!(fs::read(db.media_file(&result.audio_filename).unwrap()).unwrap(),b"ID3");let _=fs::remove_dir_all(root);}
#[test]fn deleting_a_song_removes_only_its_managed_media(){
 let root=std::env::temp_dir().join(format!("mm3-delete-test-{}",uuid::Uuid::now_v7()));
 let db=Library::open_at(root.join("library.sqlite"),root.join("media")).unwrap();
 let result=db.import_generated_song(GeneratedSongInput{title:Some("Delete me".into()),metadata:serde_json::Value::Null,caption:"c".into(),lyrics:"l".into(),generation_settings:serde_json::Value::Null,replay_request:None,audio_codes:None,engine_id:"test".into(),profile_id:None,source:"test".into(),audio_extension:"mp3",audio:b"ID3-delete".to_vec()}).unwrap();
 let audio=db.media_file(&result.audio_filename).unwrap();
 let cover=db.media_dir().join(format!("{}-cover.png",result.song.id));
 fs::write(&cover,[0x89,b'P',b'N',b'G',0x0D,0x0A,0x1A,0x0A]).unwrap();
 let mut song=db.get_song(&result.song.id).unwrap().unwrap();
 song.metadata=serde_json::json!({"cover_filename":cover.file_name().unwrap().to_string_lossy(),"cover_media_type":"image/png"});
 db.save_song(&song).unwrap();
 let stem=db.media_dir().join(format!("{}-vocals.wav",result.song.id));
 fs::write(&stem,b"stem").unwrap();
 let unrelated=db.media_dir().join("unrelated.wav");fs::write(&unrelated,b"keep").unwrap();
 assert!(db.delete_song_with_media(&result.song.id).unwrap());
 assert!(db.get_song(&result.song.id).unwrap().is_none());
 assert!(!audio.exists());assert!(!cover.exists());assert!(!stem.exists());assert!(unrelated.exists());
 let _=fs::remove_dir_all(root);
}
#[test]fn playlist_can_be_read_updated_and_deleted(){let root=std::env::temp_dir().join(format!("mm3-playlist-test-{}",uuid::Uuid::now_v7()));let db=Library::open_at(root.join("library.sqlite"),root.join("media")).unwrap();let song_a=db.create_song(SongInput{title:"A".into(),audio_path:None,caption:String::new(),lyrics:String::new(),metadata:serde_json::Value::Null,generation_settings:serde_json::Value::Null,engine_id:"manual".into(),profile_id:None,replay_request:None,audio_codes:None,source:"manual".into()}).unwrap().id;let song_b=db.create_song(SongInput{title:"B".into(),audio_path:None,caption:String::new(),lyrics:String::new(),metadata:serde_json::Value::Null,generation_settings:serde_json::Value::Null,engine_id:"manual".into(),profile_id:None,replay_request:None,audio_codes:None,source:"manual".into()}).unwrap().id;let created=db.create_playlist(PlaylistInput{name:"Drafts".into(),description:None,song_ids:vec![song_a]}).unwrap();let updated=db.update_playlist(&created.id,PlaylistInput{name:"Finished".into(),description:Some("native".into()),song_ids:vec![song_b.clone()]}).unwrap().unwrap();assert_eq!(updated.name,"Finished");assert_eq!(db.get_playlist(&created.id).unwrap().unwrap().song_ids,vec![song_b]);assert!(db.delete_playlist(&created.id).unwrap());assert!(db.get_playlist(&created.id).unwrap().is_none());let _=fs::remove_dir_all(root);}
#[test]fn measures_wav_duration_from_the_header_and_mp3_from_its_bitrate(){
 // 1 second of 44.1 kHz stereo 16-bit PCM.
 let mut wav=Vec::new();
 wav.extend_from_slice(b"RIFF"); wav.extend_from_slice(&0u32.to_le_bytes()); wav.extend_from_slice(b"WAVE");
 wav.extend_from_slice(b"fmt "); wav.extend_from_slice(&16u32.to_le_bytes());
 wav.extend_from_slice(&1u16.to_le_bytes()); wav.extend_from_slice(&2u16.to_le_bytes());
 wav.extend_from_slice(&44100u32.to_le_bytes()); wav.extend_from_slice(&176400u32.to_le_bytes());
 wav.extend_from_slice(&4u16.to_le_bytes()); wav.extend_from_slice(&16u16.to_le_bytes());
 wav.extend_from_slice(b"data"); wav.extend_from_slice(&176400u32.to_le_bytes());
 wav.resize(wav.len()+176400,0);
 assert!((audio_duration_seconds(&wav,"wav",None).unwrap()-1.0).abs()<0.001);
 let evidence=wav_audio_evidence(&wav).unwrap();
 assert_eq!(evidence.channels,2);
 assert_eq!(evidence.sample_rate_hz,44_100);
 assert_eq!(evidence.bits_per_sample,16);
 assert_eq!(evidence.data_bytes,176_400);
 let mut invalid=wav.clone();invalid[28..32].copy_from_slice(&1u32.to_le_bytes());
 assert!(wav_audio_evidence(&invalid).is_none());
 // 128 kbps MP3: 16 kB is one second.
 assert!((audio_duration_seconds(&vec![0u8;16000],"mp3",Some(128)).unwrap()-1.0).abs()<0.01);
 let mut tagged_mp3=b"ID3\x04\x00\x00\x00\x00\x00\x20".to_vec();tagged_mp3.resize(10+32+32000,0);
 assert!((audio_duration_seconds(&tagged_mp3,"mp3",Some(256)).unwrap()-1.0).abs()<0.01);
 assert!(audio_duration_seconds(b"not audio","wav",None).is_none());
}
#[test]fn generated_import_separates_requested_and_actual_duration(){
 let root=std::env::temp_dir().join(format!("mm3-duration-test-{}",uuid::Uuid::now_v7()));
 let db=Library::open_at(root.join("library.sqlite"),root.join("media")).unwrap();
 let mut audio=b"ID3\x04\x00\x00\x00\x00\x00\x20".to_vec();audio.resize(10+32+32000,0);
 let result=db.import_generated_song(GeneratedSongInput{
  title:Some("Duration".into()),metadata:serde_json::json!({"duration_seconds":60.0}),caption:"c".into(),lyrics:"l".into(),
  generation_settings:serde_json::json!({"payload":{"bitrate":256000}}),replay_request:None,audio_codes:None,
  engine_id:"omnibridge".into(),profile_id:None,source:"omnibridge_generation".into(),audio_extension:"mp3",audio,
 }).unwrap();
 assert_eq!(result.song.metadata["requested_duration_seconds"],60.0);
 assert!((result.song.metadata["actual_duration_seconds"].as_f64().unwrap()-1.0).abs()<0.01);
 assert_eq!(result.song.metadata["duration_seconds"],result.song.metadata["actual_duration_seconds"]);
 assert_eq!(result.song.metadata["duration_source"],"audio_file");
 let _=fs::remove_dir_all(root);
}
#[test]fn legacy_rows_publish_the_playable_duration_without_rewriting_the_database(){
 let root=std::env::temp_dir().join(format!("mm3-legacy-duration-test-{}",uuid::Uuid::now_v7()));
 let db=Library::open_at(root.join("library.sqlite"),root.join("media")).unwrap();
 fs::create_dir_all(root.join("media")).unwrap();
 let mut wav=Vec::new();
 wav.extend_from_slice(b"RIFF");wav.extend_from_slice(&0u32.to_le_bytes());wav.extend_from_slice(b"WAVE");
 wav.extend_from_slice(b"fmt ");wav.extend_from_slice(&16u32.to_le_bytes());wav.extend_from_slice(&1u16.to_le_bytes());wav.extend_from_slice(&2u16.to_le_bytes());
 wav.extend_from_slice(&44100u32.to_le_bytes());wav.extend_from_slice(&176400u32.to_le_bytes());wav.extend_from_slice(&4u16.to_le_bytes());wav.extend_from_slice(&16u16.to_le_bytes());
 wav.extend_from_slice(b"data");wav.extend_from_slice(&176400u32.to_le_bytes());wav.resize(wav.len()+176400,0);
 let audio_path=root.join("media/legacy.wav");fs::write(&audio_path,&wav).unwrap();
 let created=db.create_song(SongInput{title:"Legacy".into(),audio_path:Some(audio_path.display().to_string()),caption:"c".into(),lyrics:"l".into(),metadata:serde_json::json!({"duration_seconds":60.0}),generation_settings:serde_json::Value::Null,engine_id:"omnibridge".into(),profile_id:None,replay_request:None,audio_codes:None,source:"omnibridge_generation".into()}).unwrap();
 let hydrated=db.get_song(&created.id).unwrap().unwrap();
 assert_eq!(hydrated.metadata["requested_duration_seconds"],60.0);
 assert!((hydrated.metadata["actual_duration_seconds"].as_f64().unwrap()-1.0).abs()<0.001);
 assert_eq!(hydrated.metadata["duration_seconds"],hydrated.metadata["actual_duration_seconds"]);
 assert_eq!(hydrated.metadata["duration_source"],"audio_file");
 assert_eq!(db.list_songs().unwrap()[0].metadata["duration_source"],"audio_file");
 let persisted=db.connection.lock().unwrap().query_row("SELECT metadata_json FROM songs WHERE id=?",[&created.id],|row|row.get::<_,String>(0)).unwrap();
 assert_eq!(serde_json::from_str::<serde_json::Value>(&persisted).unwrap()["duration_seconds"],60.0);
 let _=fs::remove_dir_all(root);
}
#[test]fn a_generated_song_gets_a_readable_title_instead_of_the_whole_caption(){
 assert_eq!(generated_title("cinematic synthwave instrumental, 1980s analog synthesizers, warm bassline, soaring lead melody, polished production"),"cinematic synthwave instrumental, 1980s analog");
 assert_eq!(generated_title("Night drive. Wide synths"),"Night drive");
 assert_eq!(generated_title("   "),"Untitled track");
}}
