import path from 'path';
import { defineConfig, loadEnv } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, '.', '');
  const devApiTarget = env.MUSIC_MAKER_DEV_API_TARGET?.trim() || 'http://127.0.0.1:8765';
  const parsedDevTarget = new URL(devApiTarget);
  if (!['127.0.0.1', 'localhost', '[::1]'].includes(parsedDevTarget.hostname) || !['http:', 'https:'].includes(parsedDevTarget.protocol)) {
    throw new Error('MUSIC_MAKER_DEV_API_TARGET must be an HTTP(S) loopback URL.');
  }
  return {
    server: {
      port: 3000,
      host: '0.0.0.0',
      proxy: {
        '/v1': {
          target: devApiTarget,
          changeOrigin: true,
        },
        '/setup': {
          target: devApiTarget,
          changeOrigin: true,
        },
        '/engine': {
          target: devApiTarget,
          changeOrigin: true,
        },
        '/health': {
          target: devApiTarget,
          changeOrigin: true,
        },
        // There is deliberately no proxy for the retired ACE Node service:
        // the studio talks to the native Rust server only, so a stray legacy
        // request fails loudly in development instead of silently 500-ing.
      },
    },
    optimizeDeps: {
      exclude: ['@ffmpeg/ffmpeg', '@ffmpeg/util'],
    },
    plugins: [react()],
    resolve: {
      alias: {
        '@': path.resolve(__dirname, '.'),
      }
    }
  };
});
