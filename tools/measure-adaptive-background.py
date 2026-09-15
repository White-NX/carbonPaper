"""Windows acceptance probe using synthetic documents and local model files.

Does not open the user's database or seed scheduler qualifications. The hidden
native text control measures input/layout/scroll dispatch, not a specific Office
application's rendering or subjective smoothness.
"""
from __future__ import annotations

import argparse
import ctypes
from ctypes import wintypes as w
import json
import os
from pathlib import Path
import queue
import statistics
import struct
import subprocess
import threading
import time


ROOT = Path(__file__).resolve().parents[1]
CREATE_NO_WINDOW = 0x08000000
kernel = ctypes.WinDLL('kernel32', use_last_error=True)
kernel.CreateJobObjectW.argtypes = (ctypes.c_void_p, w.LPCWSTR)
kernel.CreateJobObjectW.restype = w.HANDLE
kernel.AssignProcessToJobObject.argtypes = (w.HANDLE, w.HANDLE)
kernel.AssignProcessToJobObject.restype = w.BOOL
kernel.SetInformationJobObject.argtypes = (w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD)
kernel.SetInformationJobObject.restype = w.BOOL
kernel.CloseHandle.argtypes = (w.HANDLE,)


class CpuInfo(ctypes.Structure):
    _fields_ = [('flags', w.DWORD), ('rate', w.DWORD)]


class Worker:
    def __init__(self, executable: Path, models: Path, ort: Path):
        env = os.environ.copy()
        env['PATH'] = str(ort.parent) + os.pathsep + env.get('PATH', '')
        self.process = subprocess.Popen([str(executable), '--models-root', str(models),
            '--onnx-models-root', str(models), '--ort-dylib', str(ort)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            creationflags=CREATE_NO_WINDOW, env=env)
        self.job = kernel.CreateJobObjectW(None, None)
        if not self.job or not kernel.AssignProcessToJobObject(self.job, int(self.process._handle)):
            self.process.kill()
            raise ctypes.WinError(ctypes.get_last_error())
        self.set_cap(5)
        self.frames: queue.Queue = queue.Queue()
        self.next_id = 0
        threading.Thread(target=self._read, daemon=True).start()
        ready = self.receive(30)
        assert ready['status'] == 'semantic_ready' and ready['protocol_version'] == 4, ready

    def set_cap(self, percent: int | None):
        info = CpuInfo(5 if percent else 0, (percent or 0) * 100)
        if not kernel.SetInformationJobObject(self.job, 15, ctypes.byref(info), ctypes.sizeof(info)):
            raise ctypes.WinError(ctypes.get_last_error())

    def _read(self):
        def exact(count):
            result = b''
            while len(result) < count:
                chunk = self.process.stdout.read(count - len(result))
                if not chunk:
                    raise EOFError('semantic worker closed its output')
                result += chunk
            return result
        try:
            while True:
                size, = struct.unpack('<I', exact(4))
                if not 0 < size <= 1024 * 1024:
                    raise ValueError('invalid semantic frame length')
                self.frames.put(json.loads(exact(size)))
        except Exception as error:
            self.frames.put(error)

    def receive(self, timeout=120):
        result = self.frames.get(timeout=timeout)
        if isinstance(result, Exception):
            raise result
        return result

    def send(self, value, body=b''):
        raw = json.dumps(value, ensure_ascii=False).encode('utf-8')
        self.process.stdin.write(struct.pack('<I', len(raw)) + raw + body)
        self.process.stdin.flush()

    def request(self, command, **fields):
        self.next_id += 1
        body = fields.pop('body', b'')
        request = {'request_id': self.next_id, 'command': command, **fields}
        self.send(request, body)
        response = self.receive()
        assert response['request_id'] == self.next_id, response
        if response['status'] == 'error':
            raise RuntimeError(str(response))
        return response

    def automatic_request(self, command, **fields):
        self.set_cap(5)
        try:
            return self.request(command, **fields)
        finally:
            self.set_cap(None)

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(timeout=5)
        kernel.CloseHandle(self.job)


def percentile(values, fraction=.95):
    return sorted(values)[max(0, int(len(values) * fraction + .999999) - 1)]


def infer(worker, model):
    if model == 'bge_reranker_v2_m3':
        return worker.request('rerank', timeout_ms=120000, model=model,
            query='整理项目设计文档', documents=['正在检查项目的设计记录、待办事项和测试结果。'] * 1)
    if model == 'chinese_clip':
        pixels = bytes([70, 100, 160]) * (224 * 224)
        return worker.request('embed_image', timeout_ms=120000, model=model,
            images=[{'width':224,'height':224,'stride':672,'offset':0,'body_len':len(pixels)}],
            body_len=len(pixels), body=pixels)
    return worker.request('embed_text', timeout_ms=120000, model=model,
        texts=['检查项目的设计文档，整理会议记录和待办事项。'])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', type=Path, default=ROOT / '.tmp_pytest/adaptive/performance.json')
    parser.add_argument('--worker', type=Path, default=ROOT / 'src-tauri/semantic-worker/target/release/carbonpaper-semantic-worker.exe')
    parser.add_argument('--samples', type=int, default=20)
    args = parser.parse_args()
    models = Path(os.environ['LOCALAPPDATA']) / 'carbonpaper/models'
    ort = ROOT / 'src-tauri/pre-bundle/onnxruntime/1.24.2/onnxruntime.dll'
    args.output.parent.mkdir(parents=True, exist_ok=True)
    report = {'cpu_limit_percent': 5, 'logical_cores': os.cpu_count(), 'samples': args.samples,
        'workload': 'synthetic Chinese text / solid RGB image; hidden native RichTextBox input and scroll',
        'qualification_cache_written': False, 'models': {}}
    worker = Worker(args.worker, models, ort)
    ui = None
    try:
        for model in ['minilm_l12', 'chinese_clip', 'bge_reranker_v2_m3']:
            cold = []
            for _ in range(3):
                worker.request('unload')
                cold.append(infer(worker, model)['timings']['model_load_ms'])
            warm = [infer(worker, model)['timings'] for _ in range(args.samples)]
            p95 = percentile([s['request_total_ms'] for s in warm])
            report['models'][model] = {'cold_samples_ms': cold, 'cold_p95_ms': percentile(cold),
                'warm_p95_ms': p95, 'warm_cpu_p95_ms': percentile([s['cpu_ms'] for s in warm]),
                'warm_eligible_for_this_input': args.samples >= 20 and p95 <= 500,
                'cold_eligible': percentile(cold) <= 1000}
            print(model, json.dumps(report['models'][model]), flush=True)

        # A deliberately longer inference gives cancellation an in-flight ORT
        # run. Late control for its ID is followed by a different request ID.
        cancellation = []
        for _ in range(20):
            worker.next_id += 1
            rid = worker.next_id
            worker.send({'command':'rerank','request_id':rid,'timeout_ms':120000,
                'model':'bge_reranker_v2_m3','query':'设计文档',
                'documents':['这是用于中断验收的测试文档。' * 100] * 8})
            time.sleep(.025)
            begin = time.perf_counter()
            worker.send({'command':'cancel','request_id':rid})
            killed = False
            try:
                reply = worker.receive(.5)
                assert reply['request_id'] == rid, reply
                assert reply.get('kind') == 'cancelled', reply
            except queue.Empty:
                worker.close()
                killed = True
            cancellation.append((time.perf_counter()-begin)*1000)
            if killed:
                worker = Worker(args.worker, models, ort)
            else:
                worker.send({'command':'cancel','request_id':rid})
            assert worker.request('ping')['status'] == 'pong'
        report['cancellation'] = {'samples_ms': cancellation, 'p95_ms': percentile(cancellation), 'target_ms':1000}

        worker.request('unload')
        infer(worker, 'minilm_l12')
        worker.set_cap(1)
        slow = [infer(worker, 'minilm_l12')['timings']['request_total_ms'] for _ in range(5)]
        report['low_cpu_quota'] = {'cpu_limit_percent':1, 'p95_ms':percentile(slow), 'usable_as_5_percent_qualification':False}
        worker.set_cap(5)

        document = args.output.parent / 'office-input-document.txt'
        document.write_text(('项目测试文档：检查段落、输入响应和滚动位置。\n' * 2500), encoding='utf-8')
        probe = args.output.parent / 'background-ui-probe.exe'
        if not probe.exists():
            subprocess.run(['powershell','-NoProfile','-ExecutionPolicy','Bypass','-File',
                str(ROOT/'scripts/measure-background-ui.ps1'),'-OutputDirectory',str(args.output.parent)],
                check=True, creationflags=CREATE_NO_WINDOW, stdout=subprocess.DEVNULL)
        ui = subprocess.Popen([str(probe),str(document)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, text=True, creationflags=CREATE_NO_WINDOW)
        assert ui.stdout.readline().strip() == 'ready'
        def probe_input():
            ui.stdin.write('probe\n'); ui.stdin.flush()
            return json.loads(ui.stdout.readline())
        report['ui_without_background'] = probe_input()
        stopped = threading.Event()
        failures = []
        def background_loop():
            try:
                while not stopped.is_set():
                    worker.set_cap(5)
                    begin = time.perf_counter(); result = infer(worker,'minilm_l12')
                    worker.set_cap(None)
                    elapsed = time.perf_counter()-begin
                    cpu_window = result['timings']['cpu_ms'] / 1000 / (os.cpu_count() * .05)
                    stopped.wait(max(elapsed*4, cpu_window-elapsed))
            except Exception as error:
                failures.append(str(error))
        background = threading.Thread(target=background_loop)
        background.start()
        report['ui_with_background'] = probe_input()
        stopped.set(); background.join(timeout=120)
        assert not background.is_alive() and not failures, failures
        report['ui_p95_delta_ms'] = report['ui_with_background']['p95_ms'] - report['ui_without_background']['p95_ms']
        report['passed_cancellation_target'] = report['cancellation']['p95_ms'] <= 1000
        args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2)+'\n',encoding='utf-8')
        print(json.dumps(report, ensure_ascii=False, indent=2), flush=True)
        if not report['passed_cancellation_target']:
            raise SystemExit('cancellation target exceeded')
    finally:
        worker.close()
        if ui:
            ui.stdin.write('quit\n'); ui.stdin.flush()
            try: ui.wait(timeout=5)
            except subprocess.TimeoutExpired: ui.kill()


if __name__ == '__main__':
    main()
