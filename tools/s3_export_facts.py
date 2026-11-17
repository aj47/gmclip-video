#!/usr/bin/env python3
"""S3-mac export facts (H-12), dev-only: python3 tools/s3_export_facts.py OUT.mp4 PLAN.json

Walks the MP4's boxes (edts per track, mvhd/mdhd durations), ffprobes the streams,
lists keyframe intervals and the 1-s peak (30 packets, decode order), and cross-
correlates each part's output audio (0.1 s in from each edge) against the plan's
audio source at the planned sample, ±20 ms, to the sample. Needs ffmpeg/ffprobe
(/opt/homebrew/bin) and numpy.
"""
import json, struct, subprocess, sys, numpy as np
FF='/opt/homebrew/bin/ffmpeg'; FP='/opt/homebrew/bin/ffprobe'
mp4, plan_path = sys.argv[1], sys.argv[2]
plan=json.load(open(plan_path)); data=open(mp4,'rb').read()
def boxes(b, off, end):
    while off+8<=end:
        sz,t=struct.unpack('>I4s',b[off:off+8]); h=8
        if sz==1: sz=struct.unpack('>Q',b[off+8:off+16])[0]; h=16
        if sz==0: sz=end-off
        yield t.decode('latin1'), off, off+h, off+sz
        off+=sz
top=list(boxes(data,0,len(data)))
print('top-level boxes', [t for t,*_ in top])
moov=[x for x in top if x[0]=='moov'][0]
for t,o,c,e in boxes(data,moov[2],moov[3]):
    if t=='mvhd':
        v=data[c]; ts,dur=(struct.unpack('>IQ',data[c+20:c+32]) if v==1 else struct.unpack('>II',data[c+12:c+20]))
        print(f'mvhd timescale {ts} duration {dur} = {dur/ts:.6f} s')
    if t=='trak':
        kids=list(boxes(data,c,e)); names=[k[0] for k in kids]
        mdia=[k for k in kids if k[0]=='mdia'][0]
        for t2,o2,c2,e2 in boxes(data,mdia[2],mdia[3]):
            if t2=='mdhd':
                v=data[c2]; ts,dur=(struct.unpack('>IQ',data[c2+20:c2+32]) if v==1 else struct.unpack('>II',data[c2+12:c2+20]))
            if t2=='hdlr': hd=data[c2+8:c2+12].decode()
        print(f'trak {hd}: children {names}; edts {"PRESENT" if "edts" in names else "absent"}; mdhd {dur}/{ts} = {dur/ts:.6f} s')
fr=plan['output']['frames']; print('plan frames', fr, '=', fr/30, 's; audio samples', fr*1600)
pr=json.loads(subprocess.run([FP,'-v','error','-show_streams','-count_packets','-of','json',mp4],capture_output=True,text=True).stdout)
for s in pr['streams']:
    print({k:s.get(k) for k in ['codec_name','profile','level','has_b_frames','width','height','start_pts','duration_ts','time_base','nb_read_packets','sample_rate','bit_rate']})
# keyframes + 1 s peak
pk=json.loads(subprocess.run([FP,'-v','error','-select_streams','v','-show_packets','-show_entries','packet=dts,size,flags','-of','json',mp4],capture_output=True,text=True).stdout)['packets']
key=[i for i,p in enumerate(pk) if 'K' in p['flags']]
print('keyframes (decode idx) first 5', key[:5], 'intervals', sorted(set(np.diff(key).tolist())))
sizes=np.array([int(p['size']) for p in pk]); peak=max(sizes[i:i+30].sum()*8 for i in range(len(sizes)-29))
print(f'1-s peak (30-packet window, decode order) {peak/1e6:.2f} Mbps; mean {sizes.sum()*8/(len(sizes)/30)/1e6:.2f} Mbps')
# per-part audio xcorr vs source
srcs={s['id']:s['path'] for s in plan['sources']}
def pcm(path, start_s, n):
    raw=subprocess.run([FF,'-v','error','-i',path,'-vn','-ac','1','-ar','48000','-af',f'atrim=start_sample={int(round(start_s*48000))}:end_sample={int(round(start_s*48000))+n}','-f','f32le','-'],capture_output=True).stdout
    return np.frombuffer(raw,np.float32).astype(float)
outall=None
lags=[]
for p in plan['parts']:
    a=p.get('audio')
    if not a: continue
    f0,f1=p['out']; n0,n1=f0*1600,f1*1600
    margin=4800
    if n1-n0<4*margin: continue
    if outall is None:
        outall=np.frombuffer(subprocess.run([FF,'-v','error','-i',mp4,'-vn','-ac','1','-f','f32le','-'],capture_output=True).stdout,np.float32).astype(float)
    o=outall[n0+margin:n1-margin]
    s=pcm(srcs[a['source']], (a['first_sample']+margin)/48000-0.02, len(o)+1920)
    # find lag L in [-960,960] maximizing corr: o[q] ~ s[q+960+L]
    best=None
    for L in range(-960,961):
        seg=s[960+L:960+L+len(o)]
        if len(seg)<len(o): continue
        c=np.dot(o,seg)/(np.linalg.norm(o)*np.linalg.norm(seg)+1e-12)
        if best is None or c>best[1]: best=(L,c)
    lags.append((p['id'],best[0],round(best[1],4)))
print('per-part audio xcorr vs source (lag samples @48k, corr):', lags)
print('max |lag|', max(abs(l[1]) for l in lags), 'samples =', max(abs(l[1]) for l in lags)/48, 'ms over', len(lags), 'parts')
