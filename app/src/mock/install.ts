// main.ts 가 맨 먼저 불러온다. 가짜 모드(--mode mock)일 때만 가짜 DB 를 끼운다 — 배포 빌드에서는 이 분기와 가짜 데이터가 통째로 빠진다.
if (import.meta.env.VITE_MOCK) {
  const { installMock } = await import("./ipc");
  installMock();
}
export {};
