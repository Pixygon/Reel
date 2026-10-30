## A glitch sting over an angled stamp

```bash
reel new sting.reel --size 1920x1080
reel add sting.reel color:101820 --duration 4
reel add sting.reel stamp.png --track overlay --at 0 --duration 4   # alpha is kept
reel pip sting.reel --clip 101 --x 0.5 --y 0.5 --scale 0.4
reel effects sting.reel --clip 101 --rotate-deg -12 --build tiles 24 --build-seconds 1.2
reel effects sting.reel --clip 100 --rgb-split 8 --glitch 3 --shake 12
```

Picture effects run in the frame server, so `reel render` and `reel frame` need
a GPU. The no-GPU graph fallback warns and leaves them out. A shake on an
adjustment clip moves everything beneath it; burned-in titles stay still.

## One episode template, every aspect

```bash
reel new ep.reel --size 1920x1080
reel add ep.reel intro.reel                          # nested: re-renders if it changes
reel add ep.reel capture.mp4 --slot capture
reel title add ep.reel --text "Episode 1" --y 0.85 --slot card
reel variant add ep.reel 9x16 --size 1080x1920

# Next week: same project, new content, both aspects.
reel slot set ep.reel capture --media capture-2.mp4
reel slot set ep.reel card --text "Episode 2"
reel render ep.reel ep2.mp4
reel render ep.reel ep2-9x16.mp4 --variant 9x16
```

