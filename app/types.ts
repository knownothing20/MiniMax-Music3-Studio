export interface Song {
  id: string;
  title: string;
  lyrics: string;
  style: string;
  coverUrl: string;
  duration: string;
  createdAt: Date;
  isGenerating?: boolean;
  /** Durable no-handle submission retained for explicit recovery or dismissal. */
  submissionUnknown?: boolean;
  jobId?: string; // Active generation job ID for cancel
  queuePosition?: number; // Position in queue (undefined = actively generating, number = waiting in queue)
  progress?: number;
  stage?: string;
  generationParams?: any;
  tags: string[];
  audioUrl?: string;
  isPublic?: boolean;
  likeCount?: number;
  viewCount?: number;
  userId?: string;
  creator?: string;
  creator_avatar?: string;
  ditModel?: string;
  lmModel?: string;
  lmBackend?: string;
  openrouterModel?: string | null;
  generationTime?: number;
  lrcContent?: string;
  bpm?: number;
  keyScale?: string;
  timeSignature?: string;
  /** Native Music3 provenance is complete enough for POST /v1/music/replay. */
  nativeReplayAvailable?: boolean;
  /** Verified OmniBridge Artifact evidence retained from native library metadata. */
  artifactSha256?: string;
  omnibridgeJobId?: string;
}

export interface Playlist {
  id: string;
  name: string;
  description?: string;
  coverUrl?: string;
  cover_url?: string;
  songIds?: string[];
  isPublic?: boolean;
  is_public?: boolean;
  user_id?: string;
  creator?: string;
  created_at?: string;
  song_count?: number;
  songs?: any[];
}

export interface Comment {
  id: string;
  songId: string;
  userId: string;
  username: string;
  content: string;
  createdAt: Date;
}

/**
 * The exact MiniMax Music3 request the native server accepts. Field names match
 * `/v1/music/jobs`, which in turn mirrors the `MM3Request` struct in the pinned
 * minimaxmusic.cpp: there is no translation layer that could silently drop a
 * control, and anything not listed here is not a real engine parameter.
 */
export interface Music3Request {
  /** Per-request execution target. Omit to retain the server-managed default. */
  execution_target?: 'auto' | 'cloud' | 'local' | 'device-local';
  caption: string;
  lyrics: string;
  duration_seconds: number;
  steps?: number;
  /** DiT (flow-matching) noise seed. Omit for a random seed. */
  seed?: number;
  /** Autoregressive language-model seed. Omit for a random seed. */
  lm_seed?: number;
  lm_cfg?: number;
  lm_top_k?: number;
  /** Songs sampled from this prompt, each with its own LM stream. */
  lm_batch_size?: number;
  /** Flow-matching variations per song, 1..9. */
  synth_batch_size?: number;
  dit_cfg?: number;
  /** Percentile peak normalisation; 0 disables clipping. WAV32 ignores it. */
  peak_clip?: number;
  output_format: 'mp3' | 'wav16' | 'wav24' | 'wav32';
  mp3_bitrate?: number;
  /** Library title only — never sent to the engine. */
  title?: string;
  /** Studio-only provenance persisted for diagnostics, never sent to providers. */
  studio_diagnostics?: Record<string, unknown>;
}

export interface Music3JobSong {
  id: string;
  audio_url: string;
  song?: {
    id: string;
    metadata?: Record<string, unknown> | null;
    source?: string;
  };
}

export interface Music3Job {
  id: string;
  engine_id?: string;
  dispatch?: string;
  status: 'queued' | 'running' | 'completed' | 'failed' | 'cancelled' | 'unknown';
  phase: string;
  message: string;
  title?: string;
  caption: string;
  lyrics: string;
  duration_seconds: number;
  generation_settings: Record<string, unknown>;
  song?: Music3JobSong;
  songs?: Music3JobSong[];
}


export interface PlayerState {
  currentSong: Song | null;
  isPlaying: boolean;
  progress: number;
  volume: number;
}

export interface User {
  id: string;
  username: string;
  createdAt: Date;
  followerCount?: number;
  followingCount?: number;
  isFollowing?: boolean;
  isAdmin?: boolean;
  avatar_url?: string;
  banner_url?: string;
}

export interface UserProfile {
  user: User;
  publicSongs: Song[];
  publicPlaylists: Playlist[];
  stats: {
    totalSongs: number;
    totalLikes: number;
  };
}

// Simplified views for ACE-Step UI
export type View = 'create' | 'library' | 'tools' | 'playlist' | 'news' | 'api-case';
