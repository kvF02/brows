// MV3 service worker placeholder — required for the extension to be accepted by
// Chromium. It does not perform any telemetry or remote communication.
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()));