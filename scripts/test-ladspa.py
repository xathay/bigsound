#!/usr/bin/env python3
"""Exercise the actual shared libraries through their public LADSPA C ABI."""
import ctypes as C
import math
from pathlib import Path
import sys


class Hint(C.Structure):
    _fields_ = [('descriptor', C.c_int), ('low', C.c_float), ('high', C.c_float)]


class Descriptor(C.Structure):
    _fields_ = [('id', C.c_ulong), ('label', C.c_char_p), ('properties', C.c_int),
                ('name', C.c_char_p), ('maker', C.c_char_p), ('copyright', C.c_char_p),
                ('count', C.c_ulong), ('ports', C.POINTER(C.c_int)),
                ('names', C.POINTER(C.c_char_p)), ('hints', C.POINTER(Hint)),
                ('data', C.c_void_p)] + [(n, C.c_void_p) for n in
                ('instantiate', 'connect', 'activate', 'run', 'run_adding',
                 'adding_gain', 'deactivate', 'cleanup')]


def validate(directory):
    cases = 0
    for name in ('bass', 'clarity', 'space', 'cross', 'loud'):
        lib = C.CDLL(str(directory / f'libbig_{name}.so'))
        lib.ladspa_descriptor.restype = C.POINTER(Descriptor)
        lib.ladspa_descriptor.argtypes = [C.c_ulong]
        descriptor = lib.ladspa_descriptor(0)
        assert descriptor and not lib.ladspa_descriptor(1)
        d = descriptor.contents
        assert d.properties & 4 and d.label and d.count > 2
        instantiate = C.CFUNCTYPE(C.c_void_p, C.POINTER(Descriptor), C.c_ulong)(d.instantiate)
        connect = C.CFUNCTYPE(None, C.c_void_p, C.c_ulong, C.POINTER(C.c_float))(d.connect)
        activate = C.CFUNCTYPE(None, C.c_void_p)(d.activate)
        run = C.CFUNCTYPE(None, C.c_void_p, C.c_ulong)(d.run)
        cleanup = C.CFUNCTYPE(None, C.c_void_p)(d.cleanup)
        for rate in (0, 1, 7999, 768001):
            assert not instantiate(descriptor, rate), f'{name}: invalid sample rate accepted'
        for rate in (8000, 44100, 48000, 96000, 192000, 384000):
            handle = instantiate(descriptor, rate)
            assert handle
            try:
                # run(0) and disconnected audio/control ports must be harmless.
                run(handle, 0)
                run(handle, 64)
                inputs = [i for i in range(d.count) if d.ports[i] == 9]
                outputs = [i for i in range(d.count) if d.ports[i] == 10]
                controls = [i for i in range(d.count) if d.ports[i] == 5]
                assert len(inputs) == len(outputs)
                buffers = [(C.c_float * 1024)() for _ in range(d.count)]
                for i in inputs + outputs:
                    connect(handle, i, buffers[i])
                # Defaults when control ports are disconnected.
                run(handle, 64)
                for i in controls:
                    connect(handle, i, buffers[i])
                    buffers[i][0] = (d.hints[i].low + d.hints[i].high) / 2
                for block in (1, 7, 64, 127, 256, 1024):
                    for special in (0.0, float('nan'), float('inf'), -float('inf'), 1e38):
                        activate(handle)
                        for port in controls:
                            buffers[port][0] = special
                        for port in inputs:
                            for j in range(block):
                                buffers[port][j] = special if j == 0 else math.sin(j * .13) * 1.5
                        run(handle, block)
                        reference = [[buffers[i][j] for j in range(block)] for i in outputs]
                        assert all(math.isfinite(x) for channel in reference for x in channel), name
                        # In-place buffers are permitted by the LADSPA descriptor.
                        activate(handle)
                        for inp, out in zip(inputs, outputs):
                            for j in range(block):
                                buffers[inp][j] = special if j == 0 else math.sin(j * .13) * 1.5
                            connect(handle, out, buffers[inp])
                        run(handle, block)
                        for channel, inp, out in zip(reference, inputs, outputs):
                            assert all(abs(channel[j] - buffers[inp][j]) < 1e-6 for j in range(block)), name
                            connect(handle, out, buffers[out])
                        cases += 1
            finally:
                cleanup(handle)
        print(f'{name}: descriptor, rate validation, null ports, controls, reset and in-place OK')
    print(f'LADSPA: {cases} block/control/rate cases passed')


if __name__ == '__main__':
    validate(Path(sys.argv[1] if len(sys.argv) > 1 else 'target/release').resolve())
