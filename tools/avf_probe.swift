// Dev-only (tools/, never shipped): decode a movie with AVFoundation, the stack QuickTime
// Player and Safari use, and report what it saw as JSON on stdout.
//
//   swift tools/avf_probe.swift in.mp4 [audio.f32]
//
// Video: every frame decoded (AVAssetReader, 420v); count, first/last PTS, and the
// format's codec/size/colour tags. Audio: every sample decoded to interleaved f32 at the
// track's rate, count and first PTS; with a second argument the PCM is written there as
// raw little-endian f32 so a test can cross-correlate it. Exit 1 on any reader failure.
import AVFoundation
import Foundation

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write((msg + "\n").data(using: .utf8)!)
    exit(1)
}

let args = CommandLine.arguments
guard args.count >= 2 else { fail("usage: avf_probe.swift <movie> [audio.f32]") }
let url = URL(fileURLWithPath: args[1])
let asset = AVURLAsset(url: url, options: [AVURLAssetPreferPreciseDurationAndTimingKey: true])

let sem = DispatchSemaphore(value: 0)
var out: [String: Any] = [:]
Task {
    do {
        let duration = try await asset.load(.duration)
        out["duration_s"] = CMTimeGetSeconds(duration)
        let tracks = try await asset.load(.tracks)
        var report: [[String: Any]] = []
        for t in tracks {
            var r: [String: Any] = [:]
            r["media_type"] = t.mediaType.rawValue
            let tr = try await t.load(.timeRange)
            r["start_s"] = CMTimeGetSeconds(tr.start)
            r["duration_s"] = CMTimeGetSeconds(tr.duration)
            let fds = try await t.load(.formatDescriptions)
            if let fd = fds.first {
                let st = CMFormatDescriptionGetMediaSubType(fd)
                let cc = String(bytes: [UInt8((st >> 24) & 0xff), UInt8((st >> 16) & 0xff), UInt8((st >> 8) & 0xff), UInt8(st & 0xff)], encoding: .ascii) ?? "?"
                r["codec"] = cc
                if t.mediaType == .video {
                    let dim = CMVideoFormatDescriptionGetDimensions(fd)
                    r["width"] = Int(dim.width)
                    r["height"] = Int(dim.height)
                    let ext = CMFormatDescriptionGetExtensions(fd) as? [String: Any] ?? [:]
                    r["primaries"] = ext[kCVImageBufferColorPrimariesKey as String] as? String ?? ""
                    r["transfer"] = ext[kCVImageBufferTransferFunctionKey as String] as? String ?? ""
                    r["matrix"] = ext[kCVImageBufferYCbCrMatrixKey as String] as? String ?? ""
                }
            }
            let reader = try AVAssetReader(asset: asset)
            let settings: [String: Any]
            if t.mediaType == .video {
                settings = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange]
            } else {
                settings = [AVFormatIDKey: kAudioFormatLinearPCM, AVLinearPCMBitDepthKey: 32,
                            AVLinearPCMIsFloatKey: true, AVLinearPCMIsBigEndianKey: false,
                            AVLinearPCMIsNonInterleaved: false]
            }
            let output = AVAssetReaderTrackOutput(track: t, outputSettings: settings)
            output.alwaysCopiesSampleData = false
            reader.add(output)
            guard reader.startReading() else { fail("startReading: \(String(describing: reader.error))") }
            var count = 0
            var first = Double.nan
            var last = Double.nan
            var pcm = Data()
            while let sb = output.copyNextSampleBuffer() {
                let pts = CMTimeGetSeconds(CMSampleBufferGetPresentationTimeStamp(sb))
                if first.isNaN { first = pts }
                last = pts
                count += CMSampleBufferGetNumSamples(sb)
                if t.mediaType == .audio, args.count >= 3, let bb = CMSampleBufferGetDataBuffer(sb) {
                    let len = CMBlockBufferGetDataLength(bb)
                    var chunk = Data(count: len)
                    chunk.withUnsafeMutableBytes { p in
                        _ = CMBlockBufferCopyDataBytes(bb, atOffset: 0, dataLength: len, destination: p.baseAddress!)
                    }
                    pcm.append(chunk)
                }
            }
            if reader.status != .completed { fail("reader status \(reader.status.rawValue): \(String(describing: reader.error))") }
            r["decoded"] = count
            r["first_pts_s"] = first
            r["last_pts_s"] = last
            if t.mediaType == .audio, args.count >= 3 {
                try pcm.write(to: URL(fileURLWithPath: args[2]))
                r["pcm_bytes"] = pcm.count
            }
            report.append(r)
        }
        out["tracks"] = report
    } catch {
        fail("error: \(error)")
    }
    sem.signal()
}
sem.wait()
let data = try! JSONSerialization.data(withJSONObject: out, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
