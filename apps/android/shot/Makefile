CXX ?= g++
ROOT := ../../..
CPP := ../app/src/main/cpp
IMGUI := $(CPP)/third_party/imgui

CXXFLAGS := -std=c++17 -O2 -Wall -Wno-unused-parameter -Istubs -I$(CPP) -I$(IMGUI) -I$(IMGUI)/backends -I.
LDFLAGS := -lglfw -lGL -ldl -lz -pthread

SRCS := shot.cpp $(CPP)/ui.cpp \
	$(IMGUI)/imgui.cpp $(IMGUI)/imgui_draw.cpp $(IMGUI)/imgui_tables.cpp $(IMGUI)/imgui_widgets.cpp \
	$(IMGUI)/backends/imgui_impl_opengl3.cpp

.PHONY: all clean
all: lfd3k-shot

lfd3k-shot: $(SRCS) imgui_impl_opengl3_loader.h
	$(CXX) $(CXXFLAGS) -o $@ $(SRCS) $(LDFLAGS)

clean:
	rm -f lfd3k-shot
