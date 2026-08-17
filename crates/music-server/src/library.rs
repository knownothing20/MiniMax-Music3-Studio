use std::{env, fs, path::{Path, PathBuf}, sync::{Arc, Mutex}};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct Library { connection: Arc<Mutex<Connection>>, media_dir: PathBuf }
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
/// derived from the declared constant bitrate.
pub fn audio_duration_seconds(audio:&[u8],extension:&str,declared_bitrate_kbps:Option<u32>)->Option<f64>{
 match extension{
  "wav"=>{
   if audio.len()<44||&audio[0..4]!=b"RIFF"||&audio[8..12]!=b"WAVE"{return None}
   let fmt=audio.windows(4).position(|w|w==b"fmt ")?;
   let channels=u16::from_le_bytes(audio.get(fmt+10..fmt+12)?.try_into().ok()?) as f64;
   let rate=u32::from_le_bytes(audio.get(fmt+12..fmt+16)?.try_into().ok()?) as f64;
   let bits=u16::from_le_bytes(audio.get(fmt+22..fmt+24)?.try_into().ok()?) as f64;
   let data=audio.windows(4).position(|w|w==b"data")?;
   let payload=u32::from_le_bytes(audio.get(data+4..data+8)?.try_into().ok()?) as f64;
   let bytes_per_second=rate*channels*(bits/8.0);
   (bytes_per_second>0.0).then(||payload/bytes_per_second)
  }
  "mp3"=>{
   let bitrate=declared_bitrate_kbps.unwrap_or(128) as f64*1000.0;
   (bitrate>0.0).then(||(audio.len() as f64*8.0)/bitrate)
  }
  _=>None,
 }
}

/// A caption is a full style prompt, not a song name. Without an explicit
/// title the library shows a readable fragment instead of the whole prompt.
fn generated_title(caption:&str)->String{
 let first=caption.split(['\n','.',';']).map(str::trim).find(|part|!part.is_empty()).unwrap_or("Untitled track");
 let mut title=String::new();
 for word in first.split_whitespace(){
  if !title.is_empty() && title.chars().count()+1+word.chars().count()>48 {break}
  if !title.is_empty(){title.push(' ')}
  title.push_str(word);
 }
 if title.is_empty(){"Untitled track".into()}else{title}
}

impl Library {
 pub fn open_default()->Result<Self>{let root=env::var_os("MINIMAX_STUDIO_DATA_ROOT").map(PathBuf::from).unwrap_or_else(||env::current_dir().unwrap_or_else(|_|PathBuf::from(".")).join("data"));Self::open_at(root.join("library.sqlite"),root.join("media"))}
 pub fn open_at(db_path:PathBuf,media_dir:PathBuf)->Result<Self>{if let Some(p)=db_path.parent(){fs::create_dir_all(p)?};let c=Connection::open(db_path)?;c.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE IF NOT EXISTS songs(id TEXT PRIMARY KEY,title TEXT NOT NULL,audio_path TEXT,caption TEXT NOT NULL,lyrics TEXT NOT NULL,metadata_json TEXT NOT NULL,generation_settings_json TEXT NOT NULL,engine_id TEXT NOT NULL,profile_id TEXT,replay_request_json TEXT,audio_codes_json TEXT,source TEXT NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL); CREATE TABLE IF NOT EXISTS playlists(id TEXT PRIMARY KEY,name TEXT NOT NULL,description TEXT,created_at TEXT NOT NULL,updated_at TEXT NOT NULL); CREATE TABLE IF NOT EXISTS playlist_songs(playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,song_id TEXT NOT NULL REFERENCES songs(id) ON DELETE CASCADE,position INTEGER NOT NULL,PRIMARY KEY(playlist_id,song_id));")?;Ok(Self{connection:Arc::new(Mutex::new(c)),media_dir})}
 pub fn list_songs(&self)->Result<Vec<Song>>{let c=self.connection.lock().unwrap();let mut s=c.prepare("SELECT id,title,audio_path,caption,lyrics,metadata_json,generation_settings_json,engine_id,profile_id,replay_request_json,audio_codes_json,source,created_at,updated_at FROM songs ORDER BY created_at DESC")?;Ok(s.query_map([],row_song)?.collect::<rusqlite::Result<_>>()?)}
 pub fn get_song(&self,id:&str)->Result<Option<Song>>{let c=self.connection.lock().unwrap();Ok(c.query_row("SELECT id,title,audio_path,caption,lyrics,metadata_json,generation_settings_json,engine_id,profile_id,replay_request_json,audio_codes_json,source,created_at,updated_at FROM songs WHERE id=?",[id],row_song).optional()?)}
 pub fn create_song(&self,input:SongInput)->Result<Song>{let now=now();let song=Song{id:uuid::Uuid::now_v7().to_string(),title:input.title,audio_path:input.audio_path,caption:input.caption,lyrics:input.lyrics,metadata:input.metadata,generation_settings:input.generation_settings,engine_id:input.engine_id,profile_id:input.profile_id,replay_request:input.replay_request,audio_codes:input.audio_codes,source:input.source,created_at:now.clone(),updated_at:now};self.save_song(&song)?;Ok(song)}
 pub fn import_generated_song(&self,input:GeneratedSongInput)->Result<ImportedSong>{
  if input.audio.is_empty(){anyhow::bail!("cannot import an empty audio result")}
  fs::create_dir_all(&self.media_dir).with_context(||format!("create media directory {}",self.media_dir.display()))?;
  let id=uuid::Uuid::now_v7().to_string();let filename=format!("{id}.{}",input.audio_extension);let target=self.media_dir.join(&filename);let temporary=self.media_dir.join(format!("{filename}.part"));
  {let mut file=fs::OpenOptions::new().create_new(true).write(true).open(&temporary)?;use std::io::Write;file.write_all(&input.audio)?;file.sync_all()?;}
  fs::rename(&temporary,&target).with_context(||format!("publish generated audio {}",target.display()))?;
  let now=now();let title=input.title.map(|t|t.trim().to_owned()).filter(|t|!t.is_empty()).unwrap_or_else(||generated_title(&input.caption));let song=Song{id,title,audio_path:Some(target.display().to_string()),caption:input.caption,lyrics:input.lyrics,metadata:input.metadata,generation_settings:input.generation_settings,engine_id:input.engine_id,profile_id:input.profile_id,replay_request:input.replay_request,audio_codes:input.audio_codes,source:input.source,created_at:now.clone(),updated_at:now};
  if let Err(error)=self.save_song(&song){let _=fs::remove_file(&target);return Err(error.context("store generated song record"));}
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
  self.media_file(filename).map(|path|(path,media_type))
 }
 pub fn media_file(&self,filename:&str)->Option<PathBuf>{if filename.is_empty()||filename.contains(['/', '\\'])||Path::new(filename).file_name().and_then(|x|x.to_str())!=Some(filename){return None}let path=self.media_dir.join(filename);path.is_file().then_some(path)}
 pub fn media_path_for_song(&self,song:&Song)->Option<PathBuf>{let candidate=PathBuf::from(song.audio_path.as_ref()?);let root=self.media_dir.canonicalize().ok()?;let resolved=candidate.canonicalize().ok()?;resolved.starts_with(root).then_some(resolved)}
 pub fn update_song(&self,id:&str,input:SongInput)->Result<Option<Song>>{let Some(mut song)=self.get_song(id)? else{return Ok(None)};song.title=input.title;if input.audio_path.is_some(){song.audio_path=input.audio_path};song.caption=input.caption;song.lyrics=input.lyrics;song.metadata=input.metadata;song.generation_settings=input.generation_settings;song.engine_id=input.engine_id;song.profile_id=input.profile_id;if input.replay_request.is_some(){song.replay_request=input.replay_request};if input.audio_codes.is_some(){song.audio_codes=input.audio_codes};song.source=input.source;song.updated_at=now();self.save_song(&song)?;Ok(Some(song))}
 pub fn delete_song(&self,id:&str)->Result<bool>{Ok(self.connection.lock().unwrap().execute("DELETE FROM songs WHERE id=?",[id])?>0)}
 fn save_song(&self,s:&Song)->Result<()> {self.connection.lock().unwrap().execute("INSERT INTO songs VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET title=excluded.title,audio_path=excluded.audio_path,caption=excluded.caption,lyrics=excluded.lyrics,metadata_json=excluded.metadata_json,generation_settings_json=excluded.generation_settings_json,engine_id=excluded.engine_id,profile_id=excluded.profile_id,replay_request_json=excluded.replay_request_json,audio_codes_json=excluded.audio_codes_json,source=excluded.source,updated_at=excluded.updated_at",params![s.id,s.title,s.audio_path,s.caption,s.lyrics,s.metadata.to_string(),s.generation_settings.to_string(),s.engine_id,s.profile_id,s.replay_request.as_ref().map(|v|v.to_string()),s.audio_codes.as_ref().map(|v|v.to_string()),s.source,s.created_at,s.updated_at])?;Ok(())}
 pub fn list_playlists(&self)->Result<Vec<Playlist>>{let c=self.connection.lock().unwrap();let mut s=c.prepare("SELECT id,name,description,created_at,updated_at FROM playlists ORDER BY created_at DESC")?;Ok(s.query_map([],|r|playlist(&c,r))?.collect::<rusqlite::Result<_>>()?)}
 pub fn get_playlist(&self,id:&str)->Result<Option<Playlist>>{let c=self.connection.lock().unwrap();c.query_row("SELECT id,name,description,created_at,updated_at FROM playlists WHERE id=?",[id],|r|playlist(&c,r)).optional().map_err(Into::into)}
 pub fn create_playlist(&self,input:PlaylistInput)->Result<Playlist>{let now=now();let p=Playlist{id:uuid::Uuid::now_v7().to_string(),name:input.name,description:input.description,song_ids:input.song_ids,created_at:now.clone(),updated_at:now};self.save_playlist(&p)?;Ok(p)}
 pub fn update_playlist(&self,id:&str,input:PlaylistInput)->Result<Option<Playlist>>{let Some(mut playlist)=self.get_playlist(id)? else{return Ok(None)};playlist.name=input.name;playlist.description=input.description;playlist.song_ids=input.song_ids;playlist.updated_at=now();self.save_playlist(&playlist)?;Ok(Some(playlist))}
 pub fn delete_playlist(&self,id:&str)->Result<bool>{Ok(self.connection.lock().unwrap().execute("DELETE FROM playlists WHERE id=?",[id])?>0)}
 fn save_playlist(&self,p:&Playlist)->Result<()> {let mut c=self.connection.lock().unwrap();let tx=c.transaction()?;tx.execute("INSERT INTO playlists VALUES(?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,description=excluded.description,updated_at=excluded.updated_at",params![p.id,p.name,p.description,p.created_at,p.updated_at])?;tx.execute("DELETE FROM playlist_songs WHERE playlist_id=?",[&p.id])?;for(pos,id)in p.song_ids.iter().enumerate(){tx.execute("INSERT INTO playlist_songs VALUES(?,?,?)",params![p.id,id,pos as i64])?;}tx.commit()?;Ok(())}
}
fn row_song(r:&rusqlite::Row)->rusqlite::Result<Song>{Ok(Song{id:r.get(0)?,title:r.get(1)?,audio_path:r.get(2)?,caption:r.get(3)?,lyrics:r.get(4)?,metadata:json(r.get::<_,String>(5)?),generation_settings:json(r.get::<_,String>(6)?),engine_id:r.get(7)?,profile_id:r.get(8)?,replay_request:r.get::<_,Option<String>>(9)?.map(json),audio_codes:r.get::<_,Option<String>>(10)?.map(json),source:r.get(11)?,created_at:r.get(12)?,updated_at:r.get(13)?})}fn json(s:String)->serde_json::Value{serde_json::from_str(&s).unwrap_or(serde_json::Value::Null)}fn playlist(c:&Connection,r:&rusqlite::Row)->rusqlite::Result<Playlist>{let id:String=r.get(0)?;let mut q=c.prepare("SELECT song_id FROM playlist_songs WHERE playlist_id=? ORDER BY position")?;let song_ids=q.query_map([&id],|x|x.get(0))?.collect::<rusqlite::Result<_>>()?;Ok(Playlist{id,name:r.get(1)?,description:r.get(2)?,song_ids,created_at:r.get(3)?,updated_at:r.get(4)?})}fn now()->String{std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs().to_string()}
#[cfg(test)]mod tests{use super::*;#[test]fn imports_audio_and_full_provenance_atomically(){let root=std::env::temp_dir().join(format!("mm3-library-test-{}",uuid::Uuid::now_v7()));let db=Library::open_at(root.join("library.sqlite"),root.join("media")).unwrap();let result=db.import_generated_song(GeneratedSongInput{title:None,metadata:serde_json::Value::Null,caption:"c".into(),lyrics:"l".into(),generation_settings:serde_json::json!({"seed":1}),replay_request:Some(serde_json::json!({"audio_codes":"1,2,3,4,5,6,7,8","seed":1})),audio_codes:Some(serde_json::json!("1,2,3,4,5,6,7,8")),engine_id:"mm".into(),profile_id:Some("recommended-light".into()),source:"local_generation".into(),audio_extension:"mp3",audio:b"ID3".to_vec()}).unwrap();assert_eq!(db.get_song(&result.song.id).unwrap().unwrap().audio_codes,Some(serde_json::json!("1,2,3,4,5,6,7,8")));assert_eq!(fs::read(db.media_file(&result.audio_filename).unwrap()).unwrap(),b"ID3");let _=fs::remove_dir_all(root);}
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
 // 128 kbps MP3: 16 kB is one second.
 assert!((audio_duration_seconds(&vec![0u8;16000],"mp3",Some(128)).unwrap()-1.0).abs()<0.01);
 assert!(audio_duration_seconds(b"not audio","wav",None).is_none());
}
#[test]fn a_generated_song_gets_a_readable_title_instead_of_the_whole_caption(){
 assert_eq!(generated_title("cinematic synthwave instrumental, 1980s analog synthesizers, warm bassline, soaring lead melody, polished production"),"cinematic synthwave instrumental, 1980s analog");
 assert_eq!(generated_title("Night drive. Wide synths"),"Night drive");
 assert_eq!(generated_title("   "),"Untitled track");
}}
