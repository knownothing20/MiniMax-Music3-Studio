import React, { useState, useEffect, useRef, useCallback } from 'react';
import { Sidebar } from './components/Sidebar';
import { CreatePanel } from './components/CreatePanel';
import { SongList } from './components/SongList';
import { RightSidebar } from './components/RightSidebar';
import { Player } from './components/Player';
import { LibraryView } from './components/LibraryView';
import { CreatePlaylistModal, AddToPlaylistModal } from './components/PlaylistModals';
import { CoverRegenModal } from './components/CoverRegenModal';
import { ReplayModal } from './components/ReplayModal';
import { VideoGeneratorModal } from './components/VideoGeneratorModal';
import { SettingsModal } from './components/SettingsModal';
import { Song, Music3Request, Music3Job, View, Playlist } from './types';
// Resizable panel hook
function useResizablePanel(key: string, defaultWidth: number, min: number, max: number, direction: 'left' | 'right' = 'left') {
  const [width, setWidth] = React.useState(() => {
    const saved = localStorage.getItem(`panel-${key}`);
    return saved ? Number(saved) : defaultWidth;
  });

  const onMouseDown = React.useCallback((e: React.MouseEvent) => {
    const startX = e.clientX;
    const startW = width;
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';

    const onMouseMove = (ev: MouseEvent) => {
      const delta = ev.clientX - startX;
      const newW = Math.min(max, Math.max(min, startW + (direction === 'left' ? delta : -delta)));
      setWidth(newW);
    };
    const onMouseUp = (ev: MouseEvent) => {
      const delta = ev.clientX - startX;
      const finalW = Math.min(max, Math.max(min, startW + (direction === 'left' ? delta : -delta)));
      localStorage.setItem(`panel-${key}`, String(finalW));
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      document.removeEventListener('mousemove', onMouseMove);
      document.removeEventListener('mouseup', onMouseUp);
    };
    document.addEventListener('mousemove', onMouseMove);
    document.addEventListener('mouseup', onMouseUp);
  }, [width, key, min, max, direction]);

  const handle = (
    <div
      onMouseDown={onMouseDown}
      className="hidden md:flex w-[5px] flex-shrink-0 items-center justify-center cursor-col-resize group z-20 relative bg-zinc-200/50 dark:bg-zinc-800 hover:bg-pink-500/30 transition-colors"
    >
      <div className="w-[3px] h-10 rounded-full bg-zinc-400/30 dark:bg-zinc-600/50 group-hover:bg-pink-500 transition-colors" />
    </div>
  );

  return { width, handle };
}
import { getAudioUrl } from './services/api';
import { useAuth } from './context/AuthContext';
import { useResponsive } from './context/ResponsiveContext';
import { I18nProvider, useI18n } from './context/I18nContext';
import { List } from 'lucide-react';
import { PlaylistDetail } from './components/PlaylistDetail';
import { Toast, ToastType } from './components/Toast';
import { SearchPage } from './components/SearchPage';
import { NewsPage } from './components/NewsPage';
import { ConfirmDialog } from './components/ConfirmDialog';
import { SetupGate } from './components/SetupGate';
import { EngineStarting } from './components/EngineStarting';
import { StudioOffline } from './components/StudioOffline';
import { StudioToolsPanel } from './components/StudioToolsPanel';
import { createNativePlaylist, deleteNativeSong, loadNativeLibrarySongs, loadNativePlaylists, updateNativePlaylist } from './services/nativeLibrary';

const NATIVE_LIKED_SONG_IDS_KEY = 'minimax-music3-native-liked-song-ids';
const SUBMISSION_UNKNOWN_MESSAGE = '提交状态未知，禁止自动重试；请恢复查询或人工确认后再处理。';
const CANCEL_RECOVERY_MESSAGE = '远程任务未确认取消，已保留恢复卡；请继续查询，禁止重新提交。';
const POLL_BASE_DELAY_MS = 1500;
const POLL_MAX_DELAY_MS = 30_000;

type ActiveMusicJob = {
  tempId: string;
  pollInterval?: ReturnType<typeof setTimeout>;
  submissionUnknown?: boolean;
};

type CancelJobResult = {
  ok: boolean;
  message?: string;
};

function isSubmissionUnknown(job: Pick<Music3Job, 'status' | 'phase'>): boolean {
  const phase = typeof job.phase === 'string' ? job.phase.toLowerCase() : '';
  return job.status === 'unknown' || phase === 'unknown' || phase === 'submission_unknown';
}

function loadNativeLikedSongIds(): Set<string> {
  try {
    const stored = JSON.parse(localStorage.getItem(NATIVE_LIKED_SONG_IDS_KEY) || '[]');
    return new Set(Array.isArray(stored) ? stored.filter((id): id is string => typeof id === 'string') : []);
  } catch {
    return new Set();
  }
}

function saveNativeLikedSongIds(ids: Set<string>): void {
  localStorage.setItem(NATIVE_LIKED_SONG_IDS_KEY, JSON.stringify([...ids]));
}

function NativeUnavailableView({ title, detail }: { title: string; detail: string }): React.ReactElement {
  return (
    <div className="flex h-full min-h-0 flex-1 items-center justify-center overflow-y-auto bg-white px-6 py-10 dark:bg-suno">
      <section className="w-full max-w-xl rounded-2xl border border-amber-500/30 bg-amber-500/5 p-6 text-center shadow-sm">
        <p className="text-xs font-semibold uppercase tracking-[0.16em] text-amber-600 dark:text-amber-400">Native Music3</p>
        <h1 className="mt-2 text-xl font-bold text-zinc-950 dark:text-white">{title}</h1>
        <p className="mt-3 text-sm leading-6 text-zinc-600 dark:text-zinc-300">{detail}</p>
      </section>
    </div>
  );
}


function AppContent() {
  // i18n
  const { t } = useI18n();

  // Responsive
  const { isMobile, isDesktop } = useResponsive();

  // Auth
  const { user } = useAuth();
  const leftPanel = useResizablePanel('create', 420, 320, 600);
  const rightPanel = useResizablePanel('details', 400, 320, 600, 'right');
  const [nativeSetupReady, setNativeSetupReady] = useState(false);
  // A track sent here from a menu's "separate into stems".
  const [stemsSongId, setStemsSongId] = useState<string | null>(null);
  // Which of the three situations the studio is in. It starts unknown, and
  // unknown must not look like "nothing is installed": showing the download
  // page for a second on every launch is how a ready studio was made to look
  // like a bill.
  const [nativeModels, setNativeModels] = useState<'unknown' | 'missing' | 'installed' | 'offline'>('unknown');
  useEffect(() => {
    const read = () => void fetch('/setup/status')
      .then((response) => (response.ok ? response.json() : Promise.reject(new Error())))
      .then((status: { ready?: boolean; engine_ready?: boolean }) => {
        setNativeModels(status.ready === true ? 'installed' : 'missing');
        if (status.ready && status.engine_ready) setNativeSetupReady(true);
      })
      // A service that does not answer is not an engine that is still coming
      // up: the application has been closed, and saying "starting" at a dead
      // process is the one thing the window must not do.
      .catch(() => {
        setNativeModels('offline');
        setNativeSetupReady(false);
      });
    read();
    const timer = window.setInterval(read, 2000);
    return () => window.clearInterval(timer);
  }, []);
  useEffect(() => {
    const open = (event: Event) => {
      setStemsSongId((event as CustomEvent<string>).detail);
      setCurrentView('tools');
    };
    const openSettings = (event: Event) => {
      setSettingsSection((event as CustomEvent<string>).detail);
      setShowSettingsModal(true);
    };
    window.addEventListener('mm3:open-stems', open);
    window.addEventListener('mm3:open-settings', openSettings);
    return () => {
      window.removeEventListener('mm3:open-stems', open);
      window.removeEventListener('mm3:open-settings', openSettings);
    };
  }, []);

  // Track multiple concurrent generation jobs
  const activeJobsRef = useRef<Map<string, ActiveMusicJob>>(new Map());
  const nativeReplayPollersRef = useRef<Map<string, ReturnType<typeof window.setInterval>>>(new Map());
  const [activeJobCount, setActiveJobCount] = useState(0);

  // FIFO drain barrier — handlers awaiting it block until the active-jobs
  // queue is empty. Used by CreatePanel to chain LLM pre-flight calls behind
  // the previous track's full completion (LLM + audio + cover) — that's the
  // user's "queue" mental model: gen N+1 starts only after gen N is done.
  const queueDrainResolversRef = useRef<Array<() => void>>([]);
  const waitForJobsToDrain = useCallback((): Promise<void> => {
    if (activeJobsRef.current.size === 0) return Promise.resolve();
    return new Promise((resolve) => {
      queueDrainResolversRef.current.push(resolve);
    });
  }, []);
  const drainQueueWaiters = useCallback(() => {
    if (activeJobsRef.current.size !== 0) return;
    const waiters = queueDrainResolversRef.current;
    queueDrainResolversRef.current = [];
    waiters.forEach((r) => r());
  }, []);

  // "Pending click" counter — bumped synchronously the moment the user
  // clicks Создать, so the button shows N/10 instantly even before the LLM
  // pre-flight completes. Decremented when the click hands off to a real
  // active job (beginPollingJob has registered it in activeJobsRef).
  const [pendingClickCount, setPendingClickCount] = useState(0);
  const incrementPendingClicks = useCallback((n = 1) => setPendingClickCount(c => c + n), []);

  // Pre-flight AbortController registry, keyed by the placeholder card's
  // tempId. CreatePanel registers a controller right before it starts the
  // OpenRouter pre-flight call; the cancel buttons (single + cancel-all)
  // pull from here to actually abort the in-flight HTTP request, otherwise
  // the user's only escape is reloading the page (the Promise chain that
  // park clicks via `waitForJobsToDrain` doesn't have an abort path of
  // its own — see handoff "Open issue #1").
  const preflightAbortersRef = useRef<Map<string, AbortController>>(new Map());
  const registerPreflightAbort = useCallback((tempId: string, ac: AbortController) => {
    preflightAbortersRef.current.set(tempId, ac);
  }, []);
  const unregisterPreflightAbort = useCallback((tempId: string) => {
    preflightAbortersRef.current.delete(tempId);
  }, []);
  const decrementPendingClicks = useCallback((n = 1) => setPendingClickCount(c => Math.max(0, c - n)), []);

  // Instant temp-song factory — called from CreatePanel at click time so the
  // user sees a card in the list IMMEDIATELY, then it's promoted with real
  // data when LLM pre-flight + POST complete. Returns the tempId so the
  // caller can stash it on the eventual `onGenerate` payload (`_tempId`).
  const createTempSongForClick = useCallback((descriptionPreview: string): string => {
    const tempId = `temp_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
    const tempSong: Song = {
      id: tempId,
      title: descriptionPreview.slice(0, 60) || (t('generating') || 'Generating…'),
      lyrics: '',
      style: '',
      coverUrl: 'https://picsum.photos/200/200?blur=10',
      duration: '--:--',
      createdAt: new Date(),
      isGenerating: true,
      // Use the i18n key — SongList renders via t(song.stage) || song.stage.
      stage: 'stageWaitingInQueue',
      tags: ['queued'],
      isPublic: true,
    };
    setSongs(prev => [tempSong, ...prev]);
    return tempId;
  }, [t]);

  // Update placeholder fields as LLM streams data, e.g. style/lyrics.
  const updateTempSongForClick = useCallback((tempId: string, patch: Partial<Song>) => {
    setSongs(prev => prev.map(s => s.id === tempId ? { ...s, ...patch } : s));
  }, []);

  // Failure path — drop the placeholder so the user doesn't see a stuck "Queued…"
  // BUT only if the card is still a placeholder (no `jobId` yet). Once App.tsx
  // handleGenerate has POSTed and beginPollingJob set jobId on the song, the
  // card represents a real running backend job — wiping it would leave the
  // user with audio gen running invisibly. Skip in that case.
  const removeTempSongForClick = useCallback((tempId: string) => {
    setSongs(prev => prev.filter(s => {
      if (s.id !== tempId) return true;
      // Promoted to active job → keep
      if (s.jobId) return true;
      return false;
    }));
  }, []);

  // Theme State
  // Dark is the studio's own look, not a preference inherited from the desktop:
  // the interface was drawn for it, and a light Windows was turning a music
  // studio into a spreadsheet on first launch. A user who picks light keeps it.
  const [theme, setTheme] = useState<'dark' | 'light'>(() => {
    const stored = localStorage.getItem('theme');
    return stored === 'light' ? 'light' : 'dark';
  });

  // Navigation State - default to create view
  const [currentView, setCurrentView] = useState<View>('create');

  // Content State
  const [songs, setSongs] = useState<Song[]>([]);
  const [playlists, setPlaylists] = useState<Playlist[]>([]);
  const [likedSongIds, setLikedSongIds] = useState<Set<string>>(new Set());
  const [playQueue, setPlayQueue] = useState<Song[]>([]);
  const [queueIndex, setQueueIndex] = useState(-1);

  // Selection State
  const [currentSong, setCurrentSong] = useState<Song | null>(null);
  const [selectedSong, setSelectedSong] = useState<Song | null>(null);
  const [selectedPlaylist, setSelectedPlaylist] = useState<Playlist | null>(null);

  // Player State
  const [isPlaying, setIsPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [volume, setVolume] = useState(() => {
    const stored = localStorage.getItem('volume');
    return stored ? parseFloat(stored) : 0.8;
  });
  const [playbackRate, setPlaybackRate] = useState(1.0);
  const [isShuffle, setIsShuffle] = useState(false);
  const [repeatMode, setRepeatMode] = useState<'none' | 'all' | 'one'>('all');

  // UI State
  const [isGenerating, setIsGenerating] = useState(false);
  const [showRightSidebar, setShowRightSidebar] = useState(false);
  const [showLeftSidebar, setShowLeftSidebar] = useState(() => window.innerWidth >= 768);

  useEffect(() => {
    if (isMobile) setShowLeftSidebar(false);
  }, [isMobile]);

  // Mobile UI Toggle
  const [mobileShowList, setMobileShowList] = useState(false);

  // Modals
  const [isCreatePlaylistModalOpen, setIsCreatePlaylistModalOpen] = useState(false);
  const [isAddToPlaylistModalOpen, setIsAddToPlaylistModalOpen] = useState(false);
  const [songToAddToPlaylist, setSongToAddToPlaylist] = useState<Song | null>(null);

  // Video Modal

  // Cover regen modal — manual Pollinations / upload entry from SongList row
  // and RightSidebar. Updates songs.cover_url via /api/songs/:id/regen-cover.
  const [songForCoverRegen, setSongForCoverRegen] = useState<Song | null>(null);
  const [songForReplay, setSongForReplay] = useState<Song | null>(null);
  const [songForVideo, setSongForVideo] = useState<Song | null>(null);

  // Settings Modal
  const [showSettingsModal, setShowSettingsModal] = useState(false);
  // Which settings page to land on, when something asks for a particular one.
  const [settingsSection, setSettingsSection] = useState<string | null>(null);

  // Profile View

  // Song View

  // Playlist View
  const [viewingPlaylistId, setViewingPlaylistId] = useState<string | null>(null);

  // Reuse State
  const [reuseData, setReuseData] = useState<{ song: Song, timestamp: number } | null>(null);

  const audioRef = useRef<HTMLAudioElement | null>(null);
  const selectedSongRef = useRef<Song | null>(null);
  const currentSongIdRef = useRef<string | null>(null);
  const pendingSeekRef = useRef<number | null>(null);
  const playNextRef = useRef<() => void>(() => {});

  // Mobile Details Modal State
  const [showMobileDetails, setShowMobileDetails] = useState(false);

  // Toast State
  const [toast, setToast] = useState<{ message: string; type: ToastType; isVisible: boolean }>({
    message: '',
    type: 'success',
    isVisible: false,
  });

  // Confirm Dialog State
  const [confirmDialog, setConfirmDialog] = useState<{
    title: string;
    message: string;
    onConfirm: () => void;
  } | null>(null);


  const showToast = (message: string, type: ToastType = 'success') => {
    setToast({ message, type, isVisible: true });
  };

  const closeToast = () => {
    setToast(prev => ({ ...prev, isVisible: false }));
  };

  const refreshNativeLibrary = useCallback(async (): Promise<boolean> => {
    try {
      const [nativeSongs, nativePlaylists] = await Promise.all([loadNativeLibrarySongs(), loadNativePlaylists()]);
      setSongs(prev => {
        const generatingSongs = prev.filter(song => song.isGenerating);
        return [...generatingSongs, ...nativeSongs];
      });
      setPlaylists(nativePlaylists);
      setLikedSongIds(loadNativeLikedSongIds());
      // A fresh native library is still the authoritative store. Falling back
      // to the retired ACE service when it is empty made ordinary first-run
      // actions issue requests to a server that is not part of this desktop app.
      return true;
    } catch {
      return false;
    }
  }, []);

  /// Watches a re-render job to completion and refreshes the library when the
  /// new take lands.
  const trackReplayJob = useCallback((jobId: string) => {
    showToast(t('replayQueued'));
    const poll = window.setInterval(async () => {
      try {
        const response = await fetch(`/v1/music/jobs/${encodeURIComponent(jobId)}`);
        if (!response.ok) throw new Error(`Re-render status request failed (${response.status})`);
        const status: { status?: string; message?: string } = await response.json();
        const state = status.status?.toLowerCase();
        if (!state || !['completed', 'failed', 'cancelled'].includes(state)) return;

        window.clearInterval(poll);
        nativeReplayPollersRef.current.delete(jobId);
        if (state === 'completed') {
          await refreshNativeLibrary();
          showToast(t('trackReady'));
        } else {
          showToast(status.message || `Re-render ${state}.`, 'error');
        }
      } catch (error) {
        window.clearInterval(poll);
        nativeReplayPollersRef.current.delete(jobId);
        showToast(error instanceof Error ? error.message : 'Re-render polling failed.', 'error');
      }
    }, 1500);
    nativeReplayPollersRef.current.set(jobId, poll);
  }, [refreshNativeLibrary, t]);

  const handleNativeReplay = useCallback((song: Song) => {
    if (!song.nativeReplayAvailable) return;
    setSongForReplay(song);
  }, []);

  useEffect(() => () => {
    nativeReplayPollersRef.current.forEach((poll) => window.clearInterval(poll));
    nativeReplayPollersRef.current.clear();
  }, []);

  // Keep selectedSongRef in sync for use in callbacks without stale closures
  useEffect(() => { selectedSongRef.current = selectedSong; }, [selectedSong]);

  // Cleanup active jobs on unmount
  useEffect(() => {
    return () => {
      // Clear all polling intervals when component unmounts
      activeJobsRef.current.forEach(({ pollInterval }) => {
        if (pollInterval !== undefined) clearInterval(pollInterval);
      });
      activeJobsRef.current.clear();
    };
  }, []);

  const handleShowDetails = (song: Song) => {
    setSelectedSong(song);
    setShowMobileDetails(true);
  };

  // Reuse Handler
  const handleReuse = (song: Song) => {
    setReuseData({ song, timestamp: Date.now() });
    setCurrentView('create');
    setMobileShowList(false);
  };

  // Song Update Handler
  const handleSongUpdate = (updatedSong: Song) => {
    setSongs(prev => prev.map(s => s.id === updatedSong.id ? updatedSong : s));
    if (currentSong?.id === updatedSong.id) {
      setCurrentSong(updatedSong);
    }
    if (selectedSong?.id === updatedSong.id) {
      setSelectedSong(updatedSong);
    }
  };

  // Theme Effect
  useEffect(() => {
    localStorage.setItem('theme', theme);
    if (theme === 'dark') {
      document.documentElement.classList.add('dark');
    } else {
      document.documentElement.classList.remove('dark');
    }
  }, [theme]);

  const toggleTheme = () => {
    setTheme(prev => prev === 'dark' ? 'light' : 'dark');
  };

  // URL Routing Effect
  useEffect(() => {
    const handleUrlChange = () => {
      const path = window.location.pathname;
      const params = new URLSearchParams(window.location.search);

      if (path === '/create' || path === '/') {
        setCurrentView('create');
        setMobileShowList(false);
      } else if (path === '/library') {
        setCurrentView('library');
      } else if (path.startsWith('/playlist/')) {
        const playlistId = path.substring(10);
        if (playlistId) {
          setViewingPlaylistId(playlistId);
          setCurrentView('playlist');
        }
      } else if (path === '/search') {
        setCurrentView('search');
      } else if (path === '/news') {
        setCurrentView('news');
      }
    };

    handleUrlChange();

    window.addEventListener('popstate', handleUrlChange);
    return () => window.removeEventListener('popstate', handleUrlChange);
  }, []);

  // Load the native library once at start and whenever the studio signals a
  // change (generation import, replay, audio import).
  useEffect(() => {
    void refreshNativeLibrary();
    const reload = () => { void refreshNativeLibrary(); };
    window.addEventListener('music3-library-changed', reload);
    return () => window.removeEventListener('music3-library-changed', reload);
  }, [refreshNativeLibrary]);


  // Player Logic
  const getActiveQueue = (song?: Song) => {
    if (playQueue.length > 0) return playQueue;
    if (song && songs.some(s => s.id === song.id)) return songs;
    return songs;
  };

  const playNext = useCallback(() => {
    if (!currentSong) return;
    const queue = getActiveQueue(currentSong);
    if (queue.length === 0) return;

    const currentIndex = queueIndex >= 0 && queue[queueIndex]?.id === currentSong.id
      ? queueIndex
      : queue.findIndex(s => s.id === currentSong.id);
    if (currentIndex === -1) return;

    if (repeatMode === 'one') {
      if (audioRef.current) {
        audioRef.current.currentTime = 0;
        audioRef.current.play();
      }
      return;
    }

    // Find next playable song (has audioUrl and not generating)
    const queueLen = queue.length;
    for (let i = 1; i <= queueLen; i++) {
      let nextIndex;
      if (isShuffle) {
        nextIndex = Math.floor(Math.random() * queueLen);
        if (queueLen > 1 && nextIndex === currentIndex) continue;
      } else {
        nextIndex = currentIndex + i;
        // In 'none' repeat mode, stop at end of queue
        if (repeatMode === 'none' && nextIndex >= queueLen) {
          setIsPlaying(false);
          return;
        }
        nextIndex = nextIndex % queueLen;
      }

      const candidate = queue[nextIndex];
      if (candidate.audioUrl && !candidate.isGenerating) {
        setQueueIndex(nextIndex);
        setCurrentSong(candidate);
        setIsPlaying(true);
        return;
      }
    }

    // No playable songs found
    setIsPlaying(false);
  }, [currentSong, queueIndex, isShuffle, repeatMode, playQueue, songs]);

  const playPrevious = useCallback(() => {
    if (!currentSong) return;
    const queue = getActiveQueue(currentSong);
    if (queue.length === 0) return;

    const currentIndex = queueIndex >= 0 && queue[queueIndex]?.id === currentSong.id
      ? queueIndex
      : queue.findIndex(s => s.id === currentSong.id);
    if (currentIndex === -1) return;

    if (currentTime > 3) {
      if (audioRef.current) audioRef.current.currentTime = 0;
      return;
    }

    // Find previous playable song (has audioUrl and not generating)
    const queueLen = queue.length;
    for (let i = 1; i <= queueLen; i++) {
      let prevIndex;
      if (isShuffle) {
        prevIndex = Math.floor(Math.random() * queueLen);
        if (queueLen > 1 && prevIndex === currentIndex) continue;
      } else {
        prevIndex = currentIndex - i;
        // In 'none' repeat mode, stop at beginning of queue
        if (repeatMode === 'none' && prevIndex < 0) {
          if (audioRef.current) audioRef.current.currentTime = 0;
          return;
        }
        prevIndex = (prevIndex + queueLen) % queueLen;
      }

      const candidate = queue[prevIndex];
      if (candidate.audioUrl && !candidate.isGenerating) {
        setQueueIndex(prevIndex);
        setCurrentSong(candidate);
        setIsPlaying(true);
        return;
      }
    }

    // No playable songs found
    setIsPlaying(false);
  }, [currentSong, queueIndex, currentTime, isShuffle, repeatMode, playQueue, songs]);

  useEffect(() => {
    playNextRef.current = playNext;
  }, [playNext]);

  // Audio Setup
  useEffect(() => {
    audioRef.current = new Audio();
    audioRef.current.crossOrigin = "anonymous";
    const audio = audioRef.current;
    audio.volume = volume;

    const onTimeUpdate = () => setCurrentTime(audio.currentTime);
    const applyPendingSeek = () => {
      if (pendingSeekRef.current === null) return;
      if (audio.seekable.length === 0) return;
      const target = pendingSeekRef.current;
      const safeTarget = Number.isFinite(audio.duration)
        ? Math.min(Math.max(target, 0), audio.duration)
        : Math.max(target, 0);
      audio.currentTime = safeTarget;
      setCurrentTime(safeTarget);
      pendingSeekRef.current = null;
    };

    const onLoadedMetadata = () => {
      setDuration(audio.duration);
      applyPendingSeek();
    };

    const onCanPlay = () => {
      applyPendingSeek();
    };

    const onProgress = () => {
      applyPendingSeek();
    };

    const onEnded = () => {
      playNextRef.current();
    };

    const onError = (e: Event) => {
      if (audio.error && audio.error.code !== 1) {
        console.error("Audio playback error:", audio.error);
        if (audio.error.code === 4) {
          showToast(t('songNotAvailable'), 'error');
        } else {
          showToast(t('unableToPlay'), 'error');
        }
      }
      setIsPlaying(false);
    };

    audio.addEventListener('timeupdate', onTimeUpdate);
    audio.addEventListener('loadedmetadata', onLoadedMetadata);
    audio.addEventListener('canplay', onCanPlay);
    audio.addEventListener('progress', onProgress);
    audio.addEventListener('ended', onEnded);
    audio.addEventListener('error', onError);

    return () => {
      audio.pause();
      const objectUrl = audio.dataset.studioObjectUrl;
      if (objectUrl) {
        URL.revokeObjectURL(objectUrl);
        delete audio.dataset.studioObjectUrl;
      }
      audio.removeEventListener('timeupdate', onTimeUpdate);
      audio.removeEventListener('loadedmetadata', onLoadedMetadata);
      audio.removeEventListener('canplay', onCanPlay);
      audio.removeEventListener('progress', onProgress);
      audio.removeEventListener('ended', onEnded);
      audio.removeEventListener('error', onError);
    };
  }, []);

  // Handle Playback State
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio || !currentSong?.audioUrl) return;

    const playAudio = async () => {
      try {
        await audio.play();
      } catch (err) {
        if (err instanceof Error && err.name !== 'AbortError') {
          console.error("Playback failed:", err);
          if (err.name === 'NotSupportedError') {
            showToast(t('songNotAvailable'), 'error');
          }
          setIsPlaying(false);
        }
      }
    };

    if (currentSongIdRef.current !== currentSong.id) {
      currentSongIdRef.current = currentSong.id;
      const expectedId = currentSong.id;
      void fetch(currentSong.audioUrl)
        .then(response => response.ok ? response.blob() : Promise.reject(new Error(`Audio request failed (${response.status})`)))
        .then(blob => {
          const objectUrl = URL.createObjectURL(blob);
          if (currentSongIdRef.current !== expectedId) {
            URL.revokeObjectURL(objectUrl);
            return;
          }
          const previous = audio.dataset.studioObjectUrl;
          if (previous) URL.revokeObjectURL(previous);
          audio.dataset.studioObjectUrl = objectUrl;
          audio.src = objectUrl;
          audio.load();
          if (isPlaying) void playAudio();
        })
        .catch((reason) => {
          if (reason instanceof Error && reason.name === 'AbortError') return;
          console.error('Authenticated audio load failed.');
          setIsPlaying(false);
        });
    } else {
      if (isPlaying) playAudio();
      else audio.pause();
    }
  }, [currentSong, isPlaying]);

  // Handle Volume
  useEffect(() => {
    if (audioRef.current) {
      audioRef.current.volume = volume;
    }
    localStorage.setItem('volume', String(volume));
  }, [volume]);

  // Handle Playback Rate
  useEffect(() => {
    if (audioRef.current) {
      audioRef.current.playbackRate = playbackRate;
    }
  }, [playbackRate]);

  // Spacebar play/pause
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.code !== 'Space') return;
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || (e.target as HTMLElement)?.isContentEditable) return;
      e.preventDefault();
      if (currentSong) {
        if (currentSong.audioUrl) {
          setIsPlaying(prev => !prev);
        }
      } else {
        // No song selected — play first available
        const available = songs.filter(s => s.audioUrl && !s.isGenerating);
        if (available.length > 0) {
          playSong(available[0], available);
        }
      }
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [currentSong, songs]);

  // Helper to cleanup a job and check if all jobs are done
  const cleanupJob = useCallback((jobId: string, tempId: string) => {
    const jobData = activeJobsRef.current.get(jobId);
    if (jobData) {
      if (jobData.pollInterval !== undefined) clearInterval(jobData.pollInterval);
      activeJobsRef.current.delete(jobId);
    }

    // Remove temp song
    setSongs(prev => prev.filter(s => s.id !== tempId));

    // Update active job count
    setActiveJobCount(activeJobsRef.current.size);

    // If no more active jobs, set isGenerating to false
    if (activeJobsRef.current.size === 0) {
      setIsGenerating(false);
    }
    drainQueueWaiters();
  }, []);

  // Cancel a single generation. The `id` may be either:
  //  - a backend jobId (track is past pre-flight, audio gen is running) → POST /cancel
  //  - a pre-flight tempId (still in OpenRouter LLM call, no jobId yet)  → abort the
  //    registered AbortController, drop the placeholder card, release slot
  //
  // We unify both paths under one handler because the SongList row only knows
  // `song.id` (= tempId) and `song.jobId`; a pre-flight card has tempId but
  // no jobId, so the cancel button passes whatever it has and we figure it
  // out here.
  /// One handler for both card kinds: a pre-flight placeholder only has a
  /// tempId (no engine job yet, so there is nothing to cancel remotely), while
  /// a submitted card carries the mm-server job id.
  const stopEngineJob = useCallback(async (jobId: string): Promise<CancelJobResult> => {
    try {
      const response = await fetch(`/v1/music/jobs/${encodeURIComponent(jobId)}`, { method: 'POST' });
      if (response.ok) return { ok: true };
      const body = await response.json().catch(() => ({})) as { error?: string; message?: string };
      return {
        ok: false,
        message: body.message || body.error || `Cancel request failed (${response.status})`,
      };
    } catch (error) {
      console.error('Cancel request failed.');
      return {
        ok: false,
        message: error instanceof Error ? error.message : 'Cancel request failed',
      };
    }
  }, []);

  const cancelGeneration = useCallback(async (id: string) => {
    const preflightAc = preflightAbortersRef.current.get(id);
    if (preflightAc) {
      preflightAc.abort();
      preflightAbortersRef.current.delete(id);
      setSongs(prev => prev.map(song => song.id === id ? { ...song, isGenerating: false, stage: 'cancelled' } : song));
      decrementPendingClicks(1);
      drainQueueWaiters();
      return;
    }

    const cancelled = await stopEngineJob(id);
    const jobData = activeJobsRef.current.get(id);
    if (!cancelled.ok) {
      if (jobData) {
        setSongs(prev => prev.map(song => song.id === jobData.tempId
          ? { ...song, isGenerating: true, stage: CANCEL_RECOVERY_MESSAGE }
          : song));
      }
      showToast(cancelled.message || CANCEL_RECOVERY_MESSAGE, 'error');
      return;
    }
    if (jobData) {
      if (jobData.pollInterval !== undefined) clearInterval(jobData.pollInterval);
      activeJobsRef.current.delete(id);
      setActiveJobCount(activeJobsRef.current.size);
      if (activeJobsRef.current.size === 0) setIsGenerating(false);
      drainQueueWaiters();
      setSongs(prev => prev.map(song =>
        song.id === jobData.tempId ? { ...song, isGenerating: false, stage: 'cancelled' } : song
      ));
    }
  }, [drainQueueWaiters, decrementPendingClicks, stopEngineJob]);

  /// Reset drops the card as well as the job: the engine is asked to stop, then
  /// the placeholder is removed so the list matches reality.
  const resetSingleJob = useCallback(async (id: string) => {
    const jobData = activeJobsRef.current.get(id);
    if (!jobData) {
      const aborter = preflightAbortersRef.current.get(id);
      if (aborter) {
        aborter.abort();
        preflightAbortersRef.current.delete(id);
      }
      setSongs(prev => prev.filter(song => song.id !== id));
      drainQueueWaiters();
      return;
    }

    const cancelled = await stopEngineJob(id);
    if (!cancelled.ok) {
      setSongs(prev => prev.map(song => song.id === jobData.tempId
        ? { ...song, isGenerating: true, stage: CANCEL_RECOVERY_MESSAGE }
        : song));
      showToast(cancelled.message || CANCEL_RECOVERY_MESSAGE, 'error');
      return;
    }
    if (jobData.pollInterval !== undefined) clearInterval(jobData.pollInterval);
    activeJobsRef.current.delete(id);
    setSongs(prev => prev.filter(song => song.id !== jobData.tempId));
    setActiveJobCount(activeJobsRef.current.size);
    if (activeJobsRef.current.size === 0) setIsGenerating(false);
    drainQueueWaiters();
  }, [drainQueueWaiters, stopEngineJob]);

  const cancelAllGenerations = useCallback(async () => {
    const preflightIds = new Set(preflightAbortersRef.current.keys());
    preflightAbortersRef.current.forEach(aborter => aborter.abort());
    preflightAbortersRef.current.clear();

    const running = [...activeJobsRef.current.entries()];
    const results = await Promise.all(running.map(async ([jobId, job]) => ({
      jobId,
      job,
      result: await stopEngineJob(jobId),
    })));
    const cancelled = results.filter(item => item.result.ok);
    const retained = results.filter(item => !item.result.ok);
    cancelled.forEach(({ jobId, job: { pollInterval } }) => {
      if (pollInterval !== undefined) clearInterval(pollInterval);
      activeJobsRef.current.delete(jobId);
    });
    const cancelledTempIds = new Set(cancelled.map(item => item.job.tempId));
    const retainedTempIds = new Set(retained.map(item => item.job.tempId));
    setSongs(prev => prev
      .filter(song => !cancelledTempIds.has(song.id) && !preflightIds.has(song.id))
      .map(song => retainedTempIds.has(song.id)
        ? { ...song, isGenerating: true, stage: CANCEL_RECOVERY_MESSAGE }
        : song));
    setActiveJobCount(activeJobsRef.current.size);
    setIsGenerating(activeJobsRef.current.size > 0);
    drainQueueWaiters();
    setPendingClickCount(0);
    if (retained.length > 0) showToast(CANCEL_RECOVERY_MESSAGE, 'error');
  }, [drainQueueWaiters, stopEngineJob]);

  const resetGeneration = cancelAllGenerations;

  // Refresh songs list (called when any job completes successfully)
  const refreshSongsList = useCallback(async () => {
    await refreshNativeLibrary();
  }, [refreshNativeLibrary]);

  /// Native Music3 job phases mapped onto the studio's stage labels. mm-server
  /// reports a phase rather than a percentage, so the card shows an honest
  /// stage name and an indeterminate bar instead of a fabricated progress
  /// number.
  const beginPollingJob = useCallback((jobId: string, tempId: string) => {
    if (activeJobsRef.current.has(jobId)) return;

    let consecutiveFailures = 0;
    activeJobsRef.current.set(jobId, { tempId });

    const schedulePoll = (delayMs: number) => {
      const pollInterval = window.setTimeout(async () => {
        const activeJob = activeJobsRef.current.get(jobId);
        if (!activeJob || activeJob.tempId !== tempId) return;
        try {
          const response = await fetch(`/v1/music/jobs/${encodeURIComponent(jobId)}`);
          if (!response.ok) throw new Error(`Job status request failed (${response.status})`);
          const job: Music3Job = await response.json();
          consecutiveFailures = 0;

          if (isSubmissionUnknown(job)) {
            activeJobsRef.current.set(jobId, { tempId, submissionUnknown: true });
            setSongs(prev => prev.map(song => song.id === tempId
              ? {
                ...song,
                jobId,
                isGenerating: true,
                stage: SUBMISSION_UNKNOWN_MESSAGE,
                progress: undefined,
                queuePosition: undefined,
              }
              : song));
            showToast(SUBMISSION_UNKNOWN_MESSAGE, 'error');
            return;
          }

          if (job.status === 'completed') {
            cleanupJob(jobId, tempId);
            setSongs(prev => prev.filter(song => song.id !== tempId));
            await refreshSongsList();
            const finished = job.songs?.[0] ?? job.song;
            if (finished?.id) setSelectedSong(current => current?.id === tempId ? null : current);
            showToast(job.songs && job.songs.length > 1
              ? `${job.songs.length} ${t('tracksReady') || 'tracks ready'}`
              : (t('trackReady') || 'Track ready'));
            if (window.innerWidth < 768) setMobileShowList(true);
            return;
          }
          if (job.status === 'failed' || job.status === 'cancelled') {
            cleanupJob(jobId, tempId);
            setSongs(prev => prev.filter(song => song.id !== tempId));
            showToast(job.message || `${t('generationFailed')}`, job.status === 'failed' ? 'error' : 'info');
            return;
          }
          schedulePoll(POLL_BASE_DELAY_MS);
        } catch (error) {
          consecutiveFailures += 1;
          const retryDelay = Math.min(
            POLL_BASE_DELAY_MS * (2 ** (consecutiveFailures - 1)),
            POLL_MAX_DELAY_MS,
          );
          console.error(`Polling temporarily unavailable for job ${jobId}.`);
          setSongs(prev => prev.map(song => song.id === tempId
            ? { ...song, isGenerating: true, stage: `状态查询暂时失败，${Math.ceil(retryDelay / 1000)} 秒后继续恢复查询。` }
            : song));
          if (consecutiveFailures === 1) {
            showToast(error instanceof Error ? error.message : 'Job status is temporarily unavailable', 'error');
          }
          schedulePoll(retryDelay);
        }
      }, delayMs);
      const current = activeJobsRef.current.get(jobId);
      if (current) activeJobsRef.current.set(jobId, { ...current, pollInterval });
    };

    schedulePoll(POLL_BASE_DELAY_MS);
    setActiveJobCount(activeJobsRef.current.size);
  }, [cleanupJob, refreshSongsList, t]);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const response = await fetch('/v1/music/jobs');
        if (!response.ok) return;
        const jobs: Music3Job[] = await response.json();
        const recoverable = jobs.filter(job =>
          job.id.startsWith('omnibridge-')
          && ['queued', 'running', 'unknown'].includes(job.status),
        );
        if (cancelled || recoverable.length === 0) return;
        setSongs(previous => {
          const knownJobIds = new Set(previous.map(song => song.jobId).filter(Boolean));
          const recovered = recoverable
            .filter(job => !knownJobIds.has(job.id))
            .map(job => ({
              id: `recovered-${job.id}`,
              jobId: job.id,
              title: job.title?.trim() || t('generating') || 'Generating...',
              lyrics: job.lyrics || '',
              style: job.caption || '',
              coverUrl: '',
              duration: '--:--',
              createdAt: new Date(),
              isGenerating: true,
              stage: isSubmissionUnknown(job) ? SUBMISSION_UNKNOWN_MESSAGE : job.message,
              tags: ['music3'],
            } satisfies Song));
          return [...recovered, ...previous];
        });
        recoverable.forEach(job => {
          const tempId = `recovered-${job.id}`;
          if (isSubmissionUnknown(job)) {
            activeJobsRef.current.set(job.id, { tempId, submissionUnknown: true });
          } else {
            beginPollingJob(job.id, tempId);
          }
        });
        setActiveJobCount(activeJobsRef.current.size);
        setIsGenerating(activeJobsRef.current.size > 0);
      } catch {
        // Startup recovery is retried on the next launch; never resubmit.
      }
    })();
    return () => { cancelled = true; };
  }, [beginPollingJob, t]);

  /// mm-server reports a phase, not a percentage, but its log ring counts the
  /// autoregressive frames and the flow-matching steps. Reading that gives the
  /// generating card a real progress bar instead of an invented one.
  useEffect(() => {
    if (activeJobCount === 0) return;
    let cancelled = false;

    const poll = async () => {
      try {
        const response = await fetch('/v1/engine/logs');
        if (!response.ok) return;
        const body: { lines?: string[] } = await response.json();
        const lines = body.lines ?? [];
        let progress: number | undefined;
        let stage: string | undefined;
        for (let index = lines.length - 1; index >= 0; index -= 1) {
          const frame = /\[AR\] Frame (\d+)\/(\d+)/.exec(lines[index]);
          if (frame) {
            // The autoregressive pass is roughly the first half of the work,
            // the diffusion pass the second; both are reported by the engine.
            progress = (Number(frame[1]) / Number(frame[2])) * 0.5;
            stage = 'stageGeneratingAudio';
            break;
          }
          const step = /\[DiT\] .*?(\d+)\/(\d+)/.exec(lines[index]);
          if (step) {
            progress = 0.5 + (Number(step[1]) / Number(step[2])) * 0.5;
            stage = 'stageGeneratingAudio';
            break;
          }
        }
        if (cancelled || progress === undefined) return;
        // The engine renders one job at a time, strictly in the order they were
        // submitted, and its log reports that one job's frames. mm-server marks
        // every queued job "running" all the same, so applying this to each
        // generating song gave them all the same bar - the exact report. Only
        // the oldest still-generating song is actually being worked on; the rest
        // wait at nought.
        setSongs(prev => {
          const generating = prev.filter(song =>
            song.isGenerating
            && song.jobId
            && !activeJobsRef.current.get(song.jobId)?.submissionUnknown);
          if (generating.length === 0) return prev;
          const active = generating.reduce((oldest, song) =>
            (song.createdAt?.getTime() ?? 0) < (oldest.createdAt?.getTime() ?? 0) ? song : oldest,
          );
          return prev.map(song => {
            if (!song.isGenerating || !song.jobId) return song;
            if (activeJobsRef.current.get(song.jobId)?.submissionUnknown) return song;
            if (song.id === active.id) return { ...song, progress, stage: stage ?? song.stage };
            return { ...song, progress: 0, stage: 'stageWaitingInQueue' };
          });
        });
      } catch {
        // Progress detail is a nicety; the job status poll remains the truth.
      }
    };

    void poll();
    const timer = window.setInterval(poll, 2000);
    return () => { cancelled = true; window.clearInterval(timer); };
  }, [activeJobCount]);

  const handleGenerate = async (params: Music3Request & { _tempId?: string }) => {
    const tempId = params._tempId || `temp_${Date.now()}_${Math.random().toString(36).slice(2, 11)}`;
    if (!params._tempId) {
      setSongs(prev => [{
        id: tempId,
        title: params.title?.trim() || t('generating') || 'Generating...',
        lyrics: params.lyrics || '',
        style: params.caption || '',
        coverUrl: '',
        duration: '--:--',
        createdAt: new Date(),
        isGenerating: true,
        stage: 'stageWaitingInQueue',
        tags: ['music3'],
      }, ...prev]);
    } else {
      setSongs(prev => prev.map(song => song.id === tempId
        ? { ...song, title: params.title?.trim() || song.title, style: params.caption || song.style, lyrics: params.lyrics || song.lyrics }
        : song));
    }

    setIsGenerating(true);
    const clientRequestId = tempId;
    const recoveryJobId = `omnibridge-${clientRequestId}`;
    setSongs(prev => prev.map(song => song.id === tempId ? { ...song, jobId: recoveryJobId } : song));
    let rejectionMessage: string | null = null;
    try {
      const { _tempId, ...request } = params;
      const response = await fetch('/v1/music/jobs', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ...request, client_request_id: clientRequestId }),
      });
      const job: Music3Job & { error?: string; message?: string } = await response.json().catch(() => ({}) as Music3Job);
      if (!response.ok || job.status === 'failed') {
        rejectionMessage = job.message || job.error || `Music3 rejected this request (${response.status})`;
        throw new Error(rejectionMessage);
      }
      if (!job.id) throw new Error('Music3 submission response did not contain a recovery handle.');
      setSongs(prev => prev.map(song => song.id === tempId ? { ...song, jobId: job.id } : song));
      if (isSubmissionUnknown(job)) {
        activeJobsRef.current.set(job.id, { tempId, submissionUnknown: true });
        setActiveJobCount(activeJobsRef.current.size);
        setSongs(prev => prev.map(song => song.id === tempId
          ? {
            ...song,
            isGenerating: true,
            stage: SUBMISSION_UNKNOWN_MESSAGE,
            progress: undefined,
            queuePosition: undefined,
          }
          : song));
        decrementPendingClicks(1);
        showToast(SUBMISSION_UNKNOWN_MESSAGE, 'error');
        return;
      }
      beginPollingJob(job.id, tempId);
      decrementPendingClicks(1);
    } catch (error) {
      if (rejectionMessage) {
        console.error('Generation request was rejected.');
        setSongs(prev => prev.filter(song => song.id !== tempId));
        if (activeJobsRef.current.size === 0) setIsGenerating(false);
        showToast(rejectionMessage, 'error');
        decrementPendingClicks(1);
        return;
      }
      console.error('Generation submission result is unknown; automatic replay is disabled.');
      beginPollingJob(recoveryJobId, tempId);
      setActiveJobCount(activeJobsRef.current.size);
      setSongs(prev => prev.map(song => song.id === tempId
        ? {
          ...song,
          jobId: recoveryJobId,
          isGenerating: true,
          stage: SUBMISSION_UNKNOWN_MESSAGE,
          progress: undefined,
          queuePosition: undefined,
        }
        : song));
      decrementPendingClicks(1);
      showToast(SUBMISSION_UNKNOWN_MESSAGE, 'error');
    }
  };


  const togglePlay = () => {
    const song = currentSong || selectedSong;
    if (!song) return;
    if (!song.audioUrl) {
      showToast(t('songNotAvailable'), 'error');
      return;
    }
    // If no currentSong yet, start playing the selected song
    if (!currentSong && song) {
      playSong(song);
      return;
    }
    setIsPlaying(!isPlaying);
  };

  const playFirst = () => {
    const available = songs.filter(s => s.audioUrl && !s.isGenerating);
    if (available.length > 0) {
      playSong(available[0], available);
    }
  };

  const playSong = (song: Song, list?: Song[]) => {
    const nextQueue = list && list.length > 0
      ? list
      : (playQueue.length > 0 && playQueue.some(s => s.id === song.id))
          ? playQueue
          : (songs.some(s => s.id === song.id) ? songs : [song]);
    const nextIndex = nextQueue.findIndex(s => s.id === song.id);
    setPlayQueue(nextQueue);
    setQueueIndex(nextIndex);

    if (currentSong?.id !== song.id) {
      const updatedSong = { ...song, viewCount: (song.viewCount || 0) + 1 };
      setCurrentSong(updatedSong);
      setSelectedSong(updatedSong);
      setIsPlaying(true);
      setSongs(prev => prev.map(s => s.id === song.id ? updatedSong : s));
    } else {
      togglePlay();
    }
    if (currentSong?.id === song.id) {
      setSelectedSong(song);
    }
    setShowRightSidebar(true);
  };

  const handleSeek = (time: number) => {
    const audio = audioRef.current;
    if (!audio) return;
    if (Number.isNaN(audio.duration) || audio.readyState < 1 || audio.seekable.length === 0) {
      pendingSeekRef.current = time;
      return;
    }
    audio.currentTime = time;
    setCurrentTime(time);
  };

  /// Favourites are a local library flag: the desktop studio has no social
  /// service, so the star is persisted next to the library instead of being
  /// posted to a server that does not exist.
  const toggleLike = (songId: string) => {
    const isLiked = likedSongIds.has(songId);
    setLikedSongIds(prev => {
      const next = new Set(prev);
      if (isLiked) next.delete(songId);
      else next.add(songId);
      saveNativeLikedSongIds(next as Set<string>);
      return next;
    });
  };

  const handleDeleteSong = (song: Song) => {
    handleDeleteSongs([song]);
  };

  const handleDeleteSongs = (songsToDelete: Song[]) => {
    if (songsToDelete.length === 0) return;

    const isSingle = songsToDelete.length === 1;
    const title = isSingle ? t('confirmDeleteTitle') : t('confirmDeleteManyTitle');
    const message = isSingle
      ? t('deleteSongConfirm').replace('{title}', songsToDelete[0].title)
      : t('deleteSongsConfirm').replace('{count}', String(songsToDelete.length));

    setConfirmDialog({
      title,
      message,
      onConfirm: async () => {
        setConfirmDialog(null);

        const idsToDelete = new Set(songsToDelete.map(song => song.id));
        const succeeded: string[] = [];
        const failed: string[] = [];

        for (const song of songsToDelete) {
          try {
            await deleteNativeSong(song.id);
            succeeded.push(song.id);
          } catch (error) {
            console.error('Failed to delete song:', error);
            failed.push(song.id);
          }
        }

        if (succeeded.length > 0) {
          setSongs(prev => prev.filter(s => !idsToDelete.has(s.id) || failed.includes(s.id)));

          setLikedSongIds(prev => {
            const next = new Set(prev);
            succeeded.forEach(id => next.delete(id));
            return next;
          });

          if (selectedSong?.id && succeeded.includes(selectedSong.id)) {
            setSelectedSong(null);
          }

          if (currentSong?.id && succeeded.includes(currentSong.id)) {
            setCurrentSong(null);
            setIsPlaying(false);
            if (audioRef.current) {
              audioRef.current.pause();
              audioRef.current.src = '';
            }
          }

          setPlayQueue(prev => prev.filter(s => !idsToDelete.has(s.id) || failed.includes(s.id)));
        }

        if (failed.length > 0) {
          showToast(t('songsDeletedPartial').replace('{succeeded}', String(succeeded.length)).replace('{total}', String(songsToDelete.length)), 'error');
        } else if (isSingle) {
          showToast(t('songDeleted'));
        } else {
          showToast(t('songsDeletedSuccess'));
        }
      },
    });
  };

  const createPlaylist = async (name: string, description: string) => {
    try {
      const playlist = await createNativePlaylist(name, description, songToAddToPlaylist ? [songToAddToPlaylist.id] : []);
      setPlaylists(prev => [playlist, ...prev]);
      if (songToAddToPlaylist) setSongToAddToPlaylist(null);
      showToast(t('playlistCreated'));
    } catch (error) {
      console.error('Create playlist error:', error);
      showToast(t('failedToCreatePlaylist'), 'error');
    }
  };

  const openAddToPlaylistModal = (song: Song) => {
    setSongToAddToPlaylist(song);
    setIsAddToPlaylistModalOpen(true);
  };

  const addSongToPlaylist = async (playlistId: string) => {
    if (!songToAddToPlaylist) return;
    try {
      const playlist = playlists.find(item => item.id === playlistId);
      if (!playlist) throw new Error('Playlist was not found in the local library');
      const songIds = Array.from(new Set([...(playlist.songIds || []), songToAddToPlaylist.id]));
      const updated = await updateNativePlaylist(playlist.id, playlist, songIds);
      setPlaylists(prev => prev.map(item => item.id === updated.id ? updated : item));
      setSongToAddToPlaylist(null);
      showToast(t('songAddedToPlaylist'));
    } catch (error) {
      console.error('Add song error:', error);
      showToast(t('failedToAddSong'), 'error');
    }
  };

  const handleNavigateToPlaylist = (playlistId: string) => {
    setViewingPlaylistId(playlistId);
    setCurrentView('playlist');
    window.history.pushState({}, '', `/playlist/${playlistId}`);
  };





  const handleBackFromPlaylist = () => {
    setViewingPlaylistId(null);
    setCurrentView('library');
    window.history.pushState({}, '', '/library');
  };

  const openCoverRegen = (song: Song) => {
    // Cover work is non-destructive, so playback deliberately keeps running.
    setSongForCoverRegen(song);
  };

  // Apply the new cover URL to local state without a full /api/songs reload
  // (the backend already wrote songs.cover_url; we just need the UI to
  // reflect it). Cache-bust by appending a timestamp so <img> re-fetches.
  const applyCoverUpdate = useCallback((songId: string, coverUrl: string) => {
    const bust = `${coverUrl}${coverUrl.includes('?') ? '&' : '?'}t=${Date.now()}`;
    setSongs(prev => prev.map(s => s.id === songId ? { ...s, coverUrl: bust } : s));
    setSelectedSong(prev => prev?.id === songId ? { ...prev, coverUrl: bust } : prev);
  }, []);

  // Covers and karaoke timings finish after the track is already on screen.
  useEffect(() => {
    const reload = () => void refreshNativeLibrary();
    window.addEventListener('mm3:library-changed', reload);
    return () => window.removeEventListener('mm3:library-changed', reload);
  }, [refreshNativeLibrary]);

  // Render Layout Logic
  const renderContent = () => {
    switch (currentView) {
      case 'tools':
        return <StudioToolsPanel initialSongId={stemsSongId} />;

      case 'library': {
        const allSongs = songs;
        return (
          <LibraryView
            allSongs={allSongs}
            likedSongs={songs.filter(s => likedSongIds.has(s.id))}
            playlists={playlists}
            onPlaySong={playSong}
            onCreatePlaylist={() => {
              setSongToAddToPlaylist(null);
              setIsCreatePlaylistModalOpen(true);
            }}
            onSelectPlaylist={(p) => handleNavigateToPlaylist(p.id)}
            onAddToPlaylist={openAddToPlaylistModal}
            onReusePrompt={handleReuse}
            onDeleteSong={handleDeleteSong}
            isNativeLibrary
            onImported={() => { void refreshNativeLibrary(); }}
          />
        );
      }

      case 'playlist':
        if (!viewingPlaylistId) return null;
        return (
          <PlaylistDetail
            playlistId={viewingPlaylistId}
            onBack={handleBackFromPlaylist}
            onPlaySong={playSong}
            onSelect={(s) => {
              setSelectedSong(s);
              setShowRightSidebar(true);
            }}
          />
        );

      case 'search':
        return (
          <SearchPage
            songs={songs}
            playlists={playlists}
            onPlaySong={playSong}
            currentSong={currentSong}
            isPlaying={isPlaying}
            onNavigateToPlaylist={handleNavigateToPlaylist}
          />
        );

      case 'news':
        return <NewsPage />;

      case 'create':
      default:
        // Two different situations, two different screens: nothing installed
        // is a decision to make, and an engine coming up is a wait to sit
        // through. They used to be the same page, which made a running studio
        // look like it owed 26 GB.
        if (nativeModels === 'offline') return <StudioOffline />;
        if (!nativeSetupReady) {
          // Until the studio has answered, and while the engine is coming up,
          // this is a wait - not a decision. The download page appears only
          // when components are genuinely missing.
          return nativeModels === 'missing'
            ? <SetupGate onReady={() => setNativeSetupReady(true)} />
            : <EngineStarting onReady={() => setNativeSetupReady(true)} />;
        }
        return (
          <div className="relative flex h-full min-h-0 min-w-0 w-full overflow-hidden">
            {/* Create Panel */}
            <div
              className={`
                ${mobileShowList ? 'hidden md:block' : 'w-full'}
                md:block min-h-0 min-w-0 flex-shrink-0 h-full bg-zinc-50 dark:bg-suno-panel relative z-10 transition-colors duration-300
              `}
              style={{ width: window.innerWidth >= 768 ? leftPanel.width : undefined }}
            >
              <CreatePanel
                onGenerate={handleGenerate}
                isGenerating={isGenerating}
                activeJobCount={activeJobCount + pendingClickCount}
                initialData={reuseData}
              />
            </div>
            {leftPanel.handle}

            {/* Song List */}
            <div className={`
              ${!mobileShowList ? 'hidden md:flex' : 'flex'}
              min-h-0 min-w-0 flex-1 flex-col h-full overflow-hidden bg-white dark:bg-suno transition-colors duration-300
            `}>
              <SongList
                songs={songs}
                currentSong={currentSong}
                selectedSong={selectedSong}
                likedSongIds={likedSongIds}
                isPlaying={isPlaying}
                onPlay={playSong}
                onSelect={(s) => {
                  setSelectedSong(s);
                  setShowRightSidebar(true);
                }}
                onToggleLike={toggleLike}
                onAddToPlaylist={openAddToPlaylistModal}
                onOpenCoverRegen={openCoverRegen}
                onShowDetails={handleShowDetails}
                onReusePrompt={handleReuse}
                onReplayMusic={handleNativeReplay}
                onExportVideo={setSongForVideo}
                onDelete={handleDeleteSong}
                onDeleteMany={handleDeleteSongs}
                onSongUpdate={handleSongUpdate}
                onCancelJob={cancelGeneration}
                onResetJob={resetSingleJob}
                onCancelAll={cancelAllGenerations}
                onResetAll={resetGeneration}
                activeJobCount={activeJobCount}
              />
            </div>

            {/* Right Sidebar */}
            {showRightSidebar && selectedSong && (
              <>
              {rightPanel.handle}
              <div
                className="hidden xl:block min-h-0 min-w-0 flex-shrink-0 h-full bg-zinc-50 dark:bg-suno-panel relative z-10 transition-colors duration-300"
                style={{ width: rightPanel.width }}
              >
                <RightSidebar
                  song={selectedSong}
                  onClose={() => setShowRightSidebar(false)}
                  onOpenCoverRegen={() => selectedSong && openCoverRegen(selectedSong)}
                  onReuse={handleReuse}
                  onReplayMusic={handleNativeReplay}
                onExportVideo={setSongForVideo}
                  onSongUpdate={handleSongUpdate}
                  isLiked={selectedSong ? likedSongIds.has(selectedSong.id) : false}
                  onToggleLike={toggleLike}
                  onDelete={handleDeleteSong}
                  onPlay={playSong}
                  isPlaying={isPlaying}
                  currentSong={currentSong}
                />
              </div>
              </>
            )}

            {/* Mobile Toggle Button */}
            <div className="md:hidden absolute top-4 right-4 z-50">
              <button
                onClick={() => setMobileShowList(!mobileShowList)}
                className="bg-zinc-800 text-white px-4 py-2 rounded-full shadow-lg border border-white/10 flex items-center gap-2 text-sm font-bold"
              >
                {mobileShowList ? t('createSong') : t('viewList')}
                <List size={16} />
              </button>
            </div>
          </div>
        );
    }
  };

  return (
    <div className="flex h-[100dvh] min-h-0 min-w-0 flex-col overflow-hidden bg-white dark:bg-suno text-zinc-900 dark:text-white font-sans antialiased selection:bg-pink-500/30 transition-colors duration-300">
      <div className="flex min-h-0 min-w-0 flex-1 overflow-hidden">
        <Sidebar
          currentView={currentView}
          onNavigate={(v) => {
            setCurrentView(v);
            if (v === 'create') {
              setMobileShowList(false);
              window.history.pushState({}, '', '/');
            } else if (v === 'library') {
              window.history.pushState({}, '', '/library');
            } else if (v === 'search') {
              window.history.pushState({}, '', '/search');
            } else if (v === 'news') {
              window.history.pushState({}, '', '/news');
            } else if (v === 'tools') {
              window.history.pushState({}, '', '/tools');
            }
            if (isMobile) setShowLeftSidebar(false);
          }}
          theme={theme}
          onToggleTheme={toggleTheme}
          user={user}
          onOpenSettings={() => setShowSettingsModal(true)}
          isOpen={showLeftSidebar}
          onToggle={() => setShowLeftSidebar(!showLeftSidebar)}
        />

        <main className="relative ml-[72px] flex min-h-0 min-w-0 flex-1 overflow-hidden md:ml-0">
          {renderContent()}
        </main>
      </div>

      {(currentSong || selectedSong) && <Player
        currentSong={currentSong || selectedSong}
        isPlaying={isPlaying}
        onTogglePlay={togglePlay}
        currentTime={currentTime}
        duration={duration}
        onSeek={handleSeek}
        onNext={playNext}
        onPrevious={playPrevious}
        volume={volume}
        onVolumeChange={setVolume}
        playbackRate={playbackRate}
        onPlaybackRateChange={setPlaybackRate}
        audioRef={audioRef}
        isShuffle={isShuffle}
        onToggleShuffle={() => setIsShuffle(!isShuffle)}
        repeatMode={repeatMode}
        onToggleRepeat={() => setRepeatMode(prev => prev === 'none' ? 'all' : prev === 'all' ? 'one' : 'none')}
        isLiked={currentSong ? likedSongIds.has(currentSong.id) : false}
        onToggleLike={() => currentSong && toggleLike(currentSong.id)}
        onReusePrompt={() => currentSong && handleReuse(currentSong)}
        onAddToPlaylist={() => currentSong && openAddToPlaylistModal(currentSong)}
        onDelete={() => currentSong && handleDeleteSong(currentSong)}
        onPlayFirst={playFirst}
      />}

      <CreatePlaylistModal
        isOpen={isCreatePlaylistModalOpen}
        onClose={() => setIsCreatePlaylistModalOpen(false)}
        onCreate={createPlaylist}
      />
      <AddToPlaylistModal
        isOpen={isAddToPlaylistModalOpen}
        onClose={() => setIsAddToPlaylistModalOpen(false)}
        playlists={playlists}
        onSelect={addSongToPlaylist}
        onCreateNew={() => {
          setIsAddToPlaylistModalOpen(false);
          setIsCreatePlaylistModalOpen(true);
        }}
      />
      <Toast
        message={toast.message}
        type={toast.type}
        isVisible={toast.isVisible}
        onClose={closeToast}
        duration={toast.type === 'error' ? 8000 : 3000}
      />
      {/* Cover regen modal — only mounted while a song is selected for regen.
          Unmounting on close revokes blob URLs (see CoverRegenModal cleanup
          effect) so generated previews don't leak across modal opens. */}
      <VideoGeneratorModal
        isOpen={Boolean(songForVideo)}
        song={songForVideo}
        onClose={() => setSongForVideo(null)}
      />
      {songForReplay && (
        <ReplayModal
          song={songForReplay}
          onClose={() => setSongForReplay(null)}
          onQueued={trackReplayJob}
        />
      )}
      {songForCoverRegen && (
        <CoverRegenModal
          song={songForCoverRegen}
          onClose={() => setSongForCoverRegen(null)}
          onCoverSaved={applyCoverUpdate}
        />
      )}
      <SettingsModal
        isOpen={showSettingsModal}
        initialSection={settingsSection}
        onClose={() => { setShowSettingsModal(false); setSettingsSection(null); }}
        theme={theme}
        onToggleTheme={toggleTheme}
      />

      {/* Mobile Details Modal */}
      {showMobileDetails && selectedSong && (
        <div className="fixed inset-0 z-[60] flex justify-end xl:hidden">
          <div
            className="absolute inset-0 bg-black/60 backdrop-blur-sm animate-in fade-in"
            onClick={() => setShowMobileDetails(false)}
          />
          <div className="relative w-full max-w-md h-full bg-zinc-50 dark:bg-suno-panel shadow-2xl animate-in slide-in-from-right duration-300 border-l border-white/10">
            <RightSidebar
              song={selectedSong}
              onClose={() => setShowMobileDetails(false)}
              onOpenCoverRegen={() => selectedSong && openCoverRegen(selectedSong)}
              onReuse={handleReuse}
              onReplayMusic={handleNativeReplay}
              onExportVideo={setSongForVideo}
              onSongUpdate={handleSongUpdate}
              isLiked={selectedSong ? likedSongIds.has(selectedSong.id) : false}
              onToggleLike={toggleLike}
              onDelete={handleDeleteSong}
              onPlay={playSong}
              isPlaying={isPlaying}
              currentSong={currentSong}
            />
          </div>
        </div>
      )}

      <ConfirmDialog
        isOpen={confirmDialog !== null}
        title={confirmDialog?.title ?? ''}
        message={confirmDialog?.message ?? ''}
        onConfirm={() => confirmDialog?.onConfirm()}
        onCancel={() => setConfirmDialog(null)}
      />
    </div>
  );
}

export default function App() {
  return (
    <I18nProvider>
      <AppContent />
    </I18nProvider>
  );
}
