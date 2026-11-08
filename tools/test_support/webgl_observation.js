// Owned, fixed-color GPU observations. Never run on an operator's website.
const hexPixels = data => Array.from(data, b => b.toString(16).padStart(2, '0')).join('');
const topPixels = (data, width, height) => {
  const top = new Uint8Array(data.length);
  for (let row = 0; row < height; row++) top.set(data.subarray(row*width*4,(row+1)*width*4),(height-row-1)*width*4);
  return top;
};
const captureGpuPixels = async kind => {
  const canvas = typeof document === 'undefined' ? new OffscreenCanvas(16,16) : document.createElement('canvas');
  canvas.width = 16; canvas.height = 16;
  const gl = canvas.getContext(kind, {alpha:true,antialias:false,premultipliedAlpha:false,preserveDrawingBuffer:true});
  if (!gl) return null;
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
      errorsPreserved:clean&&shortReadRejected&&framebufferClean,sourceUnchanged,png};
  } catch (_) {
    return {error:'fixed-gpu-observation-failed'};
  } finally { gl.getExtension('WEBGL_lose_context')?.loseContext(); }
};
const gpuProof = captureGpuPixels('webgl').then(async webgl => {
  first.gpuPixels = {webgl,webgl2:await captureGpuPixels('webgl2')};
});
