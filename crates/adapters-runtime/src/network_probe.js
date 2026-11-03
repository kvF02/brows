// Runs only inside a newly allocated, core-owned diagnostic target.
(()=>{
  const service=__BROWS_SERVICE__;
  const resultKey='__browsNetworkProbe';
  const request=async(path,body)=>{
    const response=await fetch(path,{method:'POST',body:body===undefined?undefined:JSON.stringify(body),credentials:'omit',cache:'no-store',redirect:'error',signal:AbortSignal.timeout(4000)});
    if(!response.ok)throw new Error('Probe service unavailable');
    const reader=response.body.getReader();let size=0;const parts=[];
    for(;;){const {value,done}=await reader.read();if(done)break;size+=value.length;if(size>8192){await reader.cancel();throw new Error('Probe response too large');}parts.push(value);}
    const bytes=new Uint8Array(size);let offset=0;for(const part of parts){bytes.set(part,offset);offset+=part.length;}
    return JSON.parse(new TextDecoder().decode(bytes));
  };
  (async()=>{
    const challenge=await request(service.origin+'/v1/challenge');
    if(!/^[0-9a-f]{32}$/.test(challenge.token)||challenge.hostname!==challenge.token+'.'+service.zone||!Number.isInteger(challenge.stunPort)||challenge.stunPort<service.stunMin||challenge.stunPort>service.stunMax)throw new Error('Invalid probe challenge');
    const port=new URL(service.origin).port;
    try{const ping=await fetch('https://'+challenge.hostname+(port?':'+port:'')+'/v1/ping',{credentials:'omit',redirect:'error',cache:'no-store',signal:AbortSignal.timeout(4000)});await ping.body?.cancel();}catch{}
    const srflx=[];let peer;
    try{
      peer=new RTCPeerConnection({iceServers:[{urls:'stun:'+challenge.hostname+':'+challenge.stunPort}]});
      peer.onicecandidate=event=>{if(event.candidate?.type==='srflx'&&srflx.length<32)srflx.push(event.candidate.address);};
      peer.createDataChannel('brows-network-probe');await peer.setLocalDescription(await peer.createOffer());
      await new Promise(resolve=>{const timer=setTimeout(resolve,2500);peer.onicegatheringstatechange=()=>{if(peer.iceGatheringState==='complete'){clearTimeout(timer);resolve();}};});
    }catch{}finally{peer?.close();}
    const report=await request(service.origin+'/v1/result/'+challenge.token,{srflx});
    if(!['pass','fail','not_checked'].includes(report.dns)||!['pass','fail','not_checked'].includes(report.webRtc)||!['pass','fail','not_checked'].includes(report.httpEgress))throw new Error('Invalid probe report');
    globalThis[resultKey]={dns:report.dns,webRtc:report.webRtc,httpEgress:report.httpEgress};
  })().catch(()=>{globalThis[resultKey]={dns:'not_checked',webRtc:'not_checked',httpEgress:'not_checked'};});
  return true;
})()
