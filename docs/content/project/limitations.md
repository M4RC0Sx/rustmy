---
description: "Known limitations of rustmy."
---

# Limitations

New
:   rustmy is tested extensively but has no long production history yet.
    Please [report](https://github.com/M4RC0Sx/rustmy/issues) anything that
    behaves differently from what you expect.

SQLAlchemy's asyncio extension on free-threaded Python
:   It runs through greenlet, which does not work on free-threaded builds
    yet; `rustmy.aio` itself does.

Latency-bound workloads
:   Many tiny queries, one after another, gain little: the network dominates
    them with any driver. `executemany` is the exception: it pipelines its
    statements.

CPython 3.10 installed by uv on Alpine/musl
:   It cannot load the `cp310` musllinux wheel (nor any other project's):
    CPython 3.10 names musl extensions `*-linux-gnu.so`, which uv's build of
    it does not accept. Alpine's own Python 3.10, and every later version
    from any source, work.

PyPy
:   Not supported.
