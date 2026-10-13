import { defineConfig } from "vite";

// Tauri 가 띄우는 개발 서버. 포트가 바뀌면 tauri.conf.json 의 devUrl 도 바꿔야 한다.
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true, watch: { ignored: ["**/src-tauri/**"] } },
  build: {
    target: "es2022",
    // 폐쇄망: 모든 자원을 번들에 넣는다 (CDN 금지)
    assetsInlineLimit: 0,
    chunkSizeWarningLimit: 2000,
    // 화면 둘: 메인, 데이터 흐름도 별도 창
    rollupOptions: { input: { main: "index.html", dfd: "dfd.html" } },
  },
});
