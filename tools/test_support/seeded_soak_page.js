globalThis.soakWorker = new Worker('./worker.js');
const heartbeatPath = '/soak/' + location.pathname.split('/')[2] + '/heartbeat';
function heartbeat() {
  const body = read() === first.canvas ? '' : 'canvas-changed';
  fetch(heartbeatPath, { method: 'POST', body }).catch(() => {});
}
heartbeat();
setInterval(heartbeat, 15000);
