// SPDX-License-Identifier: AGPL-3.0-or-later
// The composer's choice of the layers it sends to the bridge (layers.h).
#include <assert.h>
#include <stdio.h>

#include "../layers.h"

namespace {

const IRect kScreen = {0, 0, 720, 1280};

Layer frame(uint32_t z) {
  Layer x;
  x.used = true;
  x.type = COMPOSITION_DEVICE;
  x.buffer = {3, 720, 1280, 3072, PIXEL_FMT_RGBA_8888};
  x.region = kScreen;
  x.crop = {0, 0, 720, 1280};
  x.z = z;
  return x;
}

// The probe's clip in its XComponent.
Layer video(uint32_t z) {
  Layer x;
  x.used = true;
  x.type = COMPOSITION_DEVICE;
  x.buffer = {4, 320, 240, 512, PIXEL_FMT_YCBCR_420_SP};
  x.region = {36, 558, 648, 486};
  x.crop = {0, 0, 320, 240};
  x.z = z;
  return x;
}

Layer cursor() {
  Layer x = frame(9800);
  x.cursor = true;
  x.buffer = {5, 512, 512, 2048, PIXEL_FMT_RGBA_8888};
  x.region = {104, 456, 512, 512};
  x.crop = {0, 0, 512, 512};
  return x;
}

bool client(const Choice& c) { return c.frame < 0 && c.count == 0; }

void videoUnderTheFrame() {
  // render_service puts the video under its frame and leaves a hole there.
  const Layer layers[] = {frame(1), cursor(), video(0)};
  const Choice c = choose(layers, 3, kScreen);
  assert(c.frame == 0);
  assert(c.count == 1 && c.layers[0] == 2 && c.below == 1);
  assert(same(c.region[0], layers[2].region));
  assert(same(c.crop[0], layers[2].crop));
}

void videosInZOrder() {
  const Layer layers[] = {video(5), frame(1), video(0), video(3)};
  const Choice c = choose(layers, 4, kScreen);
  assert(c.frame == 1 && c.count == 3 && c.below == 1);
  assert(c.layers[0] == 2 && c.layers[1] == 3 && c.layers[2] == 0);
}

void frameAlone() {
  // #320's direct path: the uni-render output alone is the frame.
  const Layer layers[] = {cursor(), frame(0)};
  const Choice c = choose(layers, 2, kScreen);
  assert(c.frame == 1 && c.count == 0);
}

void clientTarget() {
  Layer layers[] = {frame(1), video(0)};
  // A transformed, translucent or RGB "video" is one more layer for GL.
  layers[1].transform = 1;
  assert(client(choose(layers, 2, kScreen)));
  layers[1] = video(0);
  layers[1].translucent = true;
  assert(client(choose(layers, 2, kScreen)));
  layers[1] = video(0);
  layers[1].buffer.format = PIXEL_FMT_RGBA_8888;
  assert(client(choose(layers, 2, kScreen)));
  // So is a video that render_service asks to compose with GL.
  layers[1] = video(0);
  layers[1].type = COMPOSITION_CLIENT;
  assert(client(choose(layers, 2, kScreen)));
  // A frame that doesn't fill the screen with its whole buffer.
  layers[1] = video(0);
  layers[0].region = {0, 0, 720, 1200};
  assert(client(choose(layers, 2, kScreen)));
  layers[0] = frame(1);
  layers[0].crop = {0, 0, 720, 1200};
  assert(client(choose(layers, 2, kScreen)));
  // Two layers that aren't video.
  const Layer two[] = {frame(1), frame(2), video(0)};
  assert(client(choose(two, 3, kScreen)));
  // Videos alone.
  const Layer videos[] = {video(0), video(1)};
  assert(client(choose(videos, 2, kScreen)));
}

void clipsToTheScreen() {
  Layer layers[] = {frame(1), video(0)};
  // A third of the clip is left of the screen.
  layers[1].region = {-100, 0, 300, 225};
  Choice c = choose(layers, 2, kScreen);
  assert(c.count == 1);
  assert(same(c.region[0], IRect{0, 0, 200, 225}));
  assert(same(c.crop[0], IRect{107, 0, 213, 240}));
  // And past the bottom.
  layers[1].region = {0, 1180, 720, 200};
  layers[1].crop = {0, 0, 320, 240};
  c = choose(layers, 2, kScreen);
  assert(same(c.region[0], IRect{0, 1180, 720, 100}));
  assert(same(c.crop[0], IRect{0, 0, 320, 120}));
  // A clip off the screen, or with a crop outside its buffer, isn't shown,
  // and the frame still is.
  layers[1].region = {0, 1280, 720, 200};
  c = choose(layers, 2, kScreen);
  assert(c.frame == 0 && c.count == 0);
  layers[1] = video(0);
  layers[1].crop = {0, 0, 321, 240};
  c = choose(layers, 2, kScreen);
  assert(c.frame == 0 && c.count == 0);
  layers[1].crop = {0, 0, 0, 240};
  c = choose(layers, 2, kScreen);
  assert(c.frame == 0 && c.count == 0);
}

void framesKeepAlphaOnlyOverVideo() {
  assert(drmFormat(PIXEL_FMT_RGBA_8888) == fourcc('X', 'B', '2', '4'));
  assert(drmFormat(PIXEL_FMT_RGBA_8888, true) == fourcc('A', 'B', '2', '4'));
  assert(drmFormat(PIXEL_FMT_BGRA_8888, true) == fourcc('A', 'R', '2', '4'));
  assert(drmFormat(PIXEL_FMT_YCBCR_420_SP) == 0);
}

}  // namespace

int main() {
  videoUnderTheFrame();
  videosInZOrder();
  frameAlone();
  clientTarget();
  clipsToTheScreen();
  framesKeepAlphaOnlyOverVideo();
  puts("ok");
  return 0;
}
