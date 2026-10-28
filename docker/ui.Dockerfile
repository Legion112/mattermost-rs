# The client, its display, its session bus and the test driver, all inside
# one container.
#
# Why: driving a GTK window needs an X display, a D-Bus session and an
# accessibility bus, and on a workstation all three are the developer's own.
# `at-spi-bus-launcher` in particular is a singleton keyed by
# XDG_RUNTIME_DIR, so a test run that shares it takes down the bus the
# desktop is using -- which on a machine somebody is working at means their
# screen reader stops. A container has its own of everything and can touch
# none of it.
#
# A Dockerfile rather than Rust, unlike the rest of this repository's
# tooling: an image is a published format with one way to describe it, and
# a hand-rolled builder would be a worse `docker build` with no upside.
#
# The client is installed from the .deb this repository already builds, so
# the UI tests exercise the package a user would install -- its dependency
# list included. A missing Depends shows up here as a window that will not
# start, rather than on somebody's machine.
FROM ubuntu:24.04

# No recommends: the image should carry what the package says it needs and
# nothing that merely tends to come with it.
#   xvfb          a display for the client to draw into
#   at-spi2-core  the accessibility bus the driver reads the widgets over
#   dbus-daemon   a session bus for that bus to be activated off
#   dbus-x11      dbus-run-session, which starts one around the test
#   ca-certificates  so the client can reach an https server, if ever asked
#   fonts-dejavu-core  or every screenshot of a failure is a grid of boxes
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      xvfb at-spi2-core dbus-daemon dbus-x11 ca-certificates fonts-dejavu-core \
 && rm -rf /var/lib/apt/lists/*

# The package and its dependencies. Copied separately from the apt layer
# above so a rebuilt .deb does not re-fetch the world.
COPY dist/*.deb /tmp/
RUN apt-get update \
 && apt-get install -y --no-install-recommends /tmp/*.deb \
 && rm -rf /var/lib/apt/lists/* /tmp/*.deb

# The driver. Built on the host against the same Ubuntu release, which is
# what `make ui-test-docker` asserts before it builds this image.
COPY target/release/mmlab /usr/bin/mmlab

# A runtime directory of its own, which is the whole point.
ENV XDG_RUNTIME_DIR=/run/mmrs
# Which measurement `perf` takes; see crates/mm-gui/src/perf.rs.
ENV MMRS_PERF_MODE=1
RUN mkdir -p /run/mmrs && chmod 700 /run/mmrs

# The driver is the entrypoint and the scenarios are the default command,
# so `docker run <image> perf --headless --guest --channel Long` measures
# scrolling in the same isolation instead.
#
# `--guest` because the server is already up: this container shares the
# lab server's network namespace, so http://localhost:8065 is it, and
# there is no Docker in here to manage anything with.
ENTRYPOINT ["/usr/bin/mmlab"]
CMD ["ui", "--headless", "--guest", "--shots", "/out"]
