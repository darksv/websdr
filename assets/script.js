const url = (location.protocol === 'https:' ? 'wss' : 'ws') + '://' + location.hostname + ':' + location.port + '/ws';
const socket = new WebSocket(url);
socket.binaryType = 'arraybuffer';
socket.addEventListener('open', function (event) {
    // socket.send('Hello Server!');
});

const maxChunkSize = 50;
let lastBlockHeight = 0;
let canvasContainer = document.getElementById('spectrogram');
let xx = 0;

socket.addEventListener('message', function (event) {
    if (!(event.data instanceof ArrayBuffer)) {
        const msg = JSON.parse(event.data);
        document.getElementById('info').innerText = msg;
        return;
    }

    if (canvasContainer.childNodes.length === 0 || lastBlockHeight >= maxChunkSize) {
        const canvas = document.createElement('canvas');
        canvas.width = 4096;
        canvas.height = maxChunkSize;
        canvasContainer.prepend(canvas);
        lastBlockHeight = 0;
    }

    const bytes = new Uint8Array(event.data);

    const canvas = canvasContainer.childNodes[0];
    const ctx = canvas.getContext('2d');
    const imageData = ctx.createImageData(4096, 1);
    // Iterate through every pixel
    for (let i = 0; i < 4096; i++) {
        imageData.data[4 * i + 0] = bytes[i];  // R value
        imageData.data[4 * i + 1] = 0;    // G value
        imageData.data[4 * i + 2] = 0;  // B value
        imageData.data[4 * i + 3] = 255;  // A value
    }
    xx++;

    ctx.putImageData(imageData, 0, maxChunkSize - lastBlockHeight);
    lastBlockHeight++;

    for (let i = canvasContainer.children.length - 1; i >= 0; i--) {
        const child = canvasContainer.children[i];
        if (child.getBoundingClientRect().top >= document.documentElement.clientHeight) {
            canvasContainer.removeChild(child);
            continue;
        }
        child.style.transform = 'translateY(' + (i * (maxChunkSize - 1) - (maxChunkSize - lastBlockHeight + 1)) + 'px)';
    }
});

let f = 93000000;

document.addEventListener('wheel', (e) => {
    f += Math.sign(e.deltaY) * 100000;
    console.log(f);
    socket.send(JSON.stringify(f));
});
