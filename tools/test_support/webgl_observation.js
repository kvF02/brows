// Owned, fixed-color GPU observations. Never run on an operator's website.
const hexPixels = data => Array.from(data, b => b.toString(16).padStart(2, '0')).join('');
const topPixels = (data, width, height) => {
  const top = new Uint8Array(data.length);
  for (let row = 0; row < height; row++) top.set(data.subarray(row*width*4,(row+1)*width*4),(height-row-1)*width*4);
  return top;
};
const capturePackLayouts = (gl, full, width) => {
  let pixelsMatch = true, outsidePreserved = true, errorsPreserved = gl.getError() === gl.NO_ERROR;
  const layouts = [[1,7,2,2],[2,7,2,2],[4,7,2,2],[8,7,2,2],[8,0,1,0]];
  try {
    for (const [alignment,rowLength,skipRows,skipPixels] of layouts) {
      gl.pixelStorei(gl.PACK_ALIGNMENT,alignment); gl.pixelStorei(gl.PACK_ROW_LENGTH,rowLength);
      gl.pixelStorei(gl.PACK_SKIP_ROWS,skipRows); gl.pixelStorei(gl.PACK_SKIP_PIXELS,skipPixels);
      const backing = new Uint8Array(180).fill(211), output = new Uint8Array(backing.buffer,7,160);
      const stride = Math.ceil((rowLength||3)*4/alignment)*alignment, first = 5+skipRows*stride+skipPixels*4;
      gl.enable(0xdead); // An existing INVALID_ENUM must survive the valid read.
      gl.readPixels(2,3,3,2,gl.RGBA,gl.UNSIGNED_BYTE,output,5);
      errorsPreserved = (gl.getError() === gl.INVALID_ENUM && gl.getError() === gl.NO_ERROR) && errorsPreserved;
      const touched = new Set();
      for (let row=0;row<2;row++) for (let col=0;col<3;col++) for (let channel=0;channel<4;channel++) {
        const index = first+row*stride+col*4+channel;
        if (output[index] !== full[((3+row)*width+2+col)*4+channel]) pixelsMatch = false;
        touched.add(7+index);
      }
      for (let i=0;i<backing.length;i++) if (!touched.has(i) && backing[i] !== 211) outsidePreserved = false;
    }
    for (const [rowLength,skipRows,skipPixels,size] of [[4,0,2,160],[7,2,2,80],[0,0,1,160]]) {
      gl.pixelStorei(gl.PACK_ROW_LENGTH,rowLength); gl.pixelStorei(gl.PACK_SKIP_ROWS,skipRows); gl.pixelStorei(gl.PACK_SKIP_PIXELS,skipPixels);
      const output = new Uint8Array(size).fill(255);
      gl.readPixels(2,3,3,2,gl.RGBA,gl.UNSIGNED_BYTE,output,5);
      errorsPreserved = (gl.getError() === gl.INVALID_OPERATION && gl.getError() === gl.NO_ERROR && output.every(v=>v===255)) && errorsPreserved;
    }
  } finally {
    gl.pixelStorei(gl.PACK_ALIGNMENT,4);
    for (const key of [gl.PACK_ROW_LENGTH,gl.PACK_SKIP_ROWS,gl.PACK_SKIP_PIXELS]) gl.pixelStorei(key,0);
  }
  return {layoutsChecked:layouts.length,pixelsMatch,outsidePreserved,errorsPreserved};
};
const captureRenderbuffer = gl => {
  const framebuffer = gl.createFramebuffer(), buffer = gl.createRenderbuffer(), unrelated = gl.createRenderbuffer();
  const texture = gl.createTexture(), copy = gl.createFramebuffer();
  try {
    gl.bindFramebuffer(gl.FRAMEBUFFER,framebuffer);
    gl.bindRenderbuffer(gl.RENDERBUFFER,buffer); gl.renderbufferStorage(gl.RENDERBUFFER,gl.RGBA8,9,7);
    gl.framebufferRenderbuffer(gl.FRAMEBUFFER,gl.COLOR_ATTACHMENT1,gl.RENDERBUFFER,buffer);
    gl.drawBuffers([gl.NONE,gl.COLOR_ATTACHMENT1]); gl.readBuffer(gl.COLOR_ATTACHMENT1);
    const complete = gl.checkFramebufferStatus(gl.FRAMEBUFFER) === gl.FRAMEBUFFER_COMPLETE;
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.bindFramebuffer(gl.DRAW_FRAMEBUFFER,null);
    gl.bindRenderbuffer(gl.RENDERBUFFER,unrelated); gl.renderbufferStorage(gl.RENDERBUFFER,gl.RGBA8,1,1);
    const full = new Uint8Array(9*7*4), repeat = new Uint8Array(full.length), crop = new Uint8Array(3*2*4);
    gl.readPixels(0,0,9,7,gl.RGBA,gl.UNSIGNED_BYTE,full);
    gl.readPixels(0,0,9,7,gl.RGBA,gl.UNSIGNED_BYTE,repeat);
    gl.readPixels(2,3,3,2,gl.RGBA,gl.UNSIGNED_BYTE,crop);
    let cropConsistent = true;
    for (let row=0;row<2;row++) for (let col=0;col<3;col++) for (let channel=0;channel<4;channel++) {
      if (crop[(row*3+col)*4+channel] !== full[((3+row)*9+2+col)*4+channel]) cropConsistent = false;
    }
    const pack = capturePackLayouts(gl,full,9);
    const bindingsRestored = gl.getParameter(gl.RENDERBUFFER_BINDING) === unrelated &&
      gl.getParameter(gl.READ_FRAMEBUFFER_BINDING) === framebuffer &&
      gl.getParameter(gl.DRAW_FRAMEBUFFER_BINDING) === null && gl.getParameter(gl.READ_BUFFER) === gl.COLOR_ATTACHMENT1;
    // A native texture copy exposes the unchanged renderbuffer source. Texture
    // FBOs deliberately remain outside the reading override's supported scope.
    gl.bindTexture(gl.TEXTURE_2D,texture); gl.copyTexImage2D(gl.TEXTURE_2D,0,gl.RGBA,0,0,9,7,0);
    gl.bindFramebuffer(gl.READ_FRAMEBUFFER,copy);
    gl.framebufferTexture2D(gl.READ_FRAMEBUFFER,gl.COLOR_ATTACHMENT0,gl.TEXTURE_2D,texture,0); gl.readBuffer(gl.COLOR_ATTACHMENT0);
    const raw = new Uint8Array(full.length); gl.readPixels(0,0,9,7,gl.RGBA,gl.UNSIGNED_BYTE,raw);
    const sourceUnchanged = hexPixels(raw) === 'a42367ff'.repeat(63);
    gl.bindFramebuffer(gl.FRAMEBUFFER,framebuffer); gl.readBuffer(gl.COLOR_ATTACHMENT1);
    gl.bindRenderbuffer(gl.RENDERBUFFER,buffer); gl.renderbufferStorage(gl.RENDERBUFFER,gl.RGBA4,9,7);
    gl.clear(gl.COLOR_BUFFER_BIT); gl.bindFramebuffer(gl.DRAW_FRAMEBUFFER,null); gl.bindRenderbuffer(gl.RENDERBUFFER,unrelated);
    const rgba4 = new Uint8Array(full.length); gl.readPixels(0,0,9,7,gl.RGBA,gl.UNSIGNED_BYTE,rgba4);
    let errorsPreserved = complete && gl.getError() === gl.NO_ERROR;
    const samples = Array.from(gl.getInternalformatParameter(gl.RENDERBUFFER,gl.RGBA8,gl.SAMPLES)).find(n=>n>0);
    if (!samples) throw new Error('positive multisample control required');
    gl.bindRenderbuffer(gl.RENDERBUFFER,buffer); gl.renderbufferStorageMultisample(gl.RENDERBUFFER,samples,gl.RGBA8,9,7);
    gl.bindRenderbuffer(gl.RENDERBUFFER,unrelated);
    const rejected = new Uint8Array(full.length).fill(255);
    gl.readPixels(0,0,9,7,gl.RGBA,gl.UNSIGNED_BYTE,rejected);
    errorsPreserved = (gl.getError() === gl.INVALID_OPERATION && gl.getError() === gl.NO_ERROR && rejected.every(v=>v===255)) && errorsPreserved;
    gl.framebufferRenderbuffer(gl.READ_FRAMEBUFFER,gl.COLOR_ATTACHMENT1,gl.RENDERBUFFER,null);
    gl.readPixels(0,0,9,7,gl.RGBA,gl.UNSIGNED_BYTE,rejected);
    errorsPreserved = (gl.getError() === gl.INVALID_FRAMEBUFFER_OPERATION && gl.getError() === gl.NO_ERROR && rejected.every(v=>v===255) && gl.getParameter(gl.RENDERBUFFER_BINDING) === unrelated) && errorsPreserved;
    const reference = typeof document === 'undefined' ? new OffscreenCanvas(9,7) : document.createElement('canvas');
    reference.width=9; reference.height=7;
    const context = reference.getContext('2d'); context.fillStyle='#a42367'; context.fillRect(0,0,9,7);
    return {pixels:hexPixels(topPixels(full,9,7)),referencePixels:hexPixels(context.getImageData(0,0,9,7).data),
      repeatStable:hexPixels(full)===hexPixels(repeat),cropConsistent,sourceUnchanged,bindingsRestored,errorsPreserved,
      nativeRgba4Pixels:hexPixels(rgba4),pack};
  } finally {
    gl.bindFramebuffer(gl.FRAMEBUFFER,null); gl.bindRenderbuffer(gl.RENDERBUFFER,null);
    gl.deleteFramebuffer(framebuffer); gl.deleteFramebuffer(copy); gl.deleteTexture(texture);
    gl.deleteRenderbuffer(buffer); gl.deleteRenderbuffer(unrelated);
  }
};
const captureGpuPixels = async kind => {
  const canvas = typeof document === 'undefined' ? new OffscreenCanvas(16,16) : document.createElement('canvas');
  canvas.width = 16; canvas.height = 16;
  const gl = canvas.getContext(kind, {alpha:true,antialias:false,premultipliedAlpha:false,preserveDrawingBuffer:true});
  if (!gl) return null;
  let stage = 'default-pixels';
  try {
    gl.clearColor(164/255,35/255,103/255,1); gl.clear(gl.COLOR_BUFFER_BIT);
    const full = new Uint8Array(1024), repeat = new Uint8Array(1024), crop = new Uint8Array(3*2*4);
    gl.readPixels(0,0,16,16,gl.RGBA,gl.UNSIGNED_BYTE,full);
    gl.readPixels(0,0,16,16,gl.RGBA,gl.UNSIGNED_BYTE,repeat);
    gl.readPixels(2,3,3,2,gl.RGBA,gl.UNSIGNED_BYTE,crop);
    const top = topPixels(full,16,16);
    let cropConsistent = true;
    for (let row = 0; row < 2; row++) for (let col = 0; col < 3; col++) for (let channel = 0; channel < 4; channel++) {
      if (crop[(row*3+col)*4+channel] !== full[((row+3)*16+col+2)*4+channel]) cropConsistent = false;
    }
    const clean = gl.getError() === gl.NO_ERROR;
    const tiny = new Uint8Array([77,77,77]);
    gl.readPixels(0,0,1,1,gl.RGBA,gl.UNSIGNED_BYTE,tiny);
    const shortReadRejected = gl.getError() === gl.INVALID_OPERATION && tiny.every(v=>v===77);
    stage = 'pack-layouts';
    const pack = kind === 'webgl2' ? capturePackLayouts(gl,full,16) : null;
    stage = 'renderbuffer';
    const renderbuffer = kind === 'webgl2' ? captureRenderbuffer(gl) : null;
    stage = 'source-copy-and-export';
    // A framebuffer copy reads the real source, outside the supported wrapper
    // scope, proving the source drawing itself was not changed by CPU reads.
    const texture = gl.createTexture(), framebuffer = gl.createFramebuffer();
    gl.bindTexture(gl.TEXTURE_2D,texture);
    gl.copyTexImage2D(gl.TEXTURE_2D,0,gl.RGBA,0,0,16,16,0);
    gl.bindFramebuffer(gl.FRAMEBUFFER,framebuffer);
    gl.framebufferTexture2D(gl.FRAMEBUFFER,gl.COLOR_ATTACHMENT0,gl.TEXTURE_2D,texture,0);
    const raw = new Uint8Array(1024);
    gl.readPixels(0,0,16,16,gl.RGBA,gl.UNSIGNED_BYTE,raw);
    const sourceUnchanged = hexPixels(raw) === 'a42367ff'.repeat(256);
    gl.bindFramebuffer(gl.FRAMEBUFFER,null);
    gl.deleteFramebuffer(framebuffer); gl.deleteTexture(texture);
    const framebufferClean = gl.getError() === gl.NO_ERROR;
    let png;
    if (typeof document !== 'undefined') png = canvas.toDataURL('image/png').split(',')[1];
    else {
      const blob = await canvas.convertToBlob({type:'image/png'});
      png = btoa(String.fromCharCode(...new Uint8Array(await blob.arrayBuffer())));
    }
    return {pixels:hexPixels(top),repeatStable:hexPixels(full)===hexPixels(repeat),cropConsistent,
      errorsPreserved:clean&&shortReadRejected&&framebufferClean,sourceUnchanged,png,pack,renderbuffer};
  } catch (_) {
    return {error:'fixed-gpu-observation-failed',stage};
  } finally { gl.getExtension('WEBGL_lose_context')?.loseContext(); }
};
const gpuProof = captureGpuPixels('webgl').then(async webgl => {
  first.gpuPixels = {webgl,webgl2:await captureGpuPixels('webgl2')};
});
