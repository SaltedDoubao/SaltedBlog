// @ts-check
import { defineConfig } from 'astro/config';
import node from '@astrojs/node';

export default defineConfig({
  output: 'server',
  adapter: node({ mode: 'standalone' }),
  server: {
    port: Number(process.env.PORT ?? 4321),
    host: true,
  },
  vite: {
    // 允许 dev 模式下读取仓库根目录的 .env（统一配置入口）
    envDir: '..',
    server: {
      // dev 模式下将浏览器发起的 /api、/uploads 请求代理到 Rust API
      proxy: {
        '/api': process.env.API_URL ?? 'http://127.0.0.1:8787',
        '/uploads': process.env.API_URL ?? 'http://127.0.0.1:8787',
      },
    },
  },
});
