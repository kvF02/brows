const endpoint = process.argv[2];
if (!endpoint?.startsWith("ws://127.0.0.1:")) {
  throw new Error("expected a loopback WebView debugger endpoint");
}

const socket = new WebSocket(endpoint);
socket.addEventListener("open", () => {
  socket.send(JSON.stringify({
    id: 1,
    method: "Runtime.evaluate",
    params: {
      expression: `JSON.stringify({
        root: document.querySelector("#root")?.innerText.slice(0, 800),
        childCount: document.querySelector("#root")?.childElementCount,
        url: location.href,
        title: document.title
      })`,
      returnByValue: true,
    },
  }));
});
socket.addEventListener("message", (event) => {
  const message = JSON.parse(event.data);
  if (message.id === 1) {
    console.log(message.result?.result?.value ?? JSON.stringify(message));
    socket.close();
  }
});
socket.addEventListener("error", () => {
  process.exitCode = 1;
});
