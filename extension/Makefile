PKGS := libnautilus-extension-4 gio-2.0 gmodule-2.0 gtk4
CFLAGS ?= -O2 -g
CFLAGS += -Wall -Wextra -Wno-unused-parameter -fPIC -fvisibility=hidden $(shell pkg-config --cflags $(PKGS))
LDLIBS := $(shell pkg-config --libs $(PKGS))
# Where the pdrive.mo catalogues are installed (see scripts/install-system.sh).
LOCALEDIR ?= /usr/local/share/locale
CFLAGS += -DLOCALEDIR='"$(LOCALEDIR)"'
EXTENSION_DIR ?= $(shell pkg-config --variable=extensiondir libnautilus-extension-4)

TARGET := libpdrive-nautilus.so

$(TARGET): pdrive-nautilus.c
	$(CC) $(CFLAGS) -shared -Wl,--no-undefined -o $@ $< $(LDLIBS)

install: $(TARGET)
	install -Dm0644 $(TARGET) $(DESTDIR)$(EXTENSION_DIR)/$(TARGET)

clean:
	rm -f $(TARGET)

.PHONY: install clean
