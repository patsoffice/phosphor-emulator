CXX ?= c++
CXXFLAGS = -std=c++17 -O2 -Wall -Wextra -I.
BINDIR = bin

# Suppress warnings from reference emulator code
MAME0148_WARN = -Wno-unused-parameter -Wno-unused-variable -Wno-unused-function \
                -Wno-deprecated-declarations -Wno-missing-field-initializers

# MAME 0.148 source paths
MAME0148_M6809  = mame0148/src/emu/cpu/m6809
MAME0148_MB88XX = mame0148/src/emu/cpu/mb88xx
MAME0148_M6800  = mame0148/src/emu/cpu/m6800
MAME0148_MCS48  = mame0148/src/emu/cpu/mcs48

all: $(BINDIR)/validate_m6809 \
     $(BINDIR)/validate_m6800 $(BINDIR)/validate_i8035 \
     $(BINDIR)/validate_mb88xx $(BINDIR)/trace_54xx

$(BINDIR):
	mkdir -p $(BINDIR)

$(BINDIR)/validate_m6809: validate_m6809.cpp mame0148_shim.h m6809_0148/emu.h m6809_0148/debugger.h \
                            $(MAME0148_M6809)/m6809.c $(MAME0148_M6809)/m6809.h \
                            $(MAME0148_M6809)/6809ops.c $(MAME0148_M6809)/6809tbl.c \
                            $(MAME0148_M6809)/6809tbl.h \
                            include/nlohmann/json.hpp | $(BINDIR)
	$(CXX) $(CXXFLAGS) $(MAME0148_WARN) -Im6809_0148 -o $@ validate_m6809.cpp

$(BINDIR)/validate_m6800: validate_m6800.cpp mame0148_shim.h m6800_0148/emu.h m6800_0148/debugger.h \
                            $(MAME0148_M6800)/m6800.c $(MAME0148_M6800)/m6800.h \
                            include/nlohmann/json.hpp | $(BINDIR)
	$(CXX) $(CXXFLAGS) $(MAME0148_WARN) -Im6800_0148 -o $@ validate_m6800.cpp

$(BINDIR)/validate_i8035: validate_i8035.cpp mame0148_shim.h mcs48_0148/emu.h mcs48_0148/debugger.h \
                            $(MAME0148_MCS48)/mcs48.c $(MAME0148_MCS48)/mcs48.h \
                            include/nlohmann/json.hpp | $(BINDIR)
	$(CXX) $(CXXFLAGS) $(MAME0148_WARN) -Imcs48_0148 -o $@ validate_i8035.cpp

$(BINDIR)/validate_mb88xx: validate_mb88xx.cpp mame0148_shim.h mb88xx/emu.h mb88xx/debugger.h \
                            $(MAME0148_MB88XX)/mb88xx.c $(MAME0148_MB88XX)/mb88xx.h \
                            include/nlohmann/json.hpp | $(BINDIR)
	$(CXX) $(CXXFLAGS) $(MAME0148_WARN) -Imb88xx -o $@ validate_mb88xx.cpp

# Not a vector validator: runs the real 54XX firmware and prints a trace to
# diff against core/tests/mb88xx_54xx_trace.rs. See that file's header.
$(BINDIR)/trace_54xx: trace_54xx.cpp mame0148_shim.h mb88xx/emu.h mb88xx/debugger.h \
                            $(MAME0148_MB88XX)/mb88xx.c $(MAME0148_MB88XX)/mb88xx.h | $(BINDIR)
	$(CXX) $(CXXFLAGS) $(MAME0148_WARN) -Imb88xx -o $@ trace_54xx.cpp

clean:
	rm -rf $(BINDIR)

.PHONY: clean all
