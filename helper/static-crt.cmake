# The Windows helper ships as one .exe with the C runtime linked in. zxing-cpp and SDL pick their
# runtime through CMake (policy CMP0091), which overrides the /MT cargo asks for: this tells them.
# Builds that link the runtime statically pass it as CMAKE_TOOLCHAIN_FILE_x86_64_pc_windows_msvc.
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded")
