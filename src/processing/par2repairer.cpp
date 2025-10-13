// C API wrapper for par2cmdline-turbo library
// This provides a simplified interface for Rust FFI

#include "../par2cmdline-turbo/src/libpar2.h"
#include <iostream>
#include <sstream>
#include <vector>
#include <string>
#include <cstring>
#include <thread>
#include <fcntl.h>
#include <unistd.h>

#ifdef __APPLE__
#include <sys/types.h>
#include <sys/sysctl.h>
#include <dirent.h>
#elif defined(__linux__)
// Linux already included unistd.h above
#include <dirent.h>
#elif defined(_WIN32)
#include <windows.h>
#include <io.h>
#endif

// C-compatible result enum matching Rust's Par2Result
extern "C" {
    enum Par2Result {
        SUCCESS = 0,
        REPAIR_POSSIBLE = 1,
        REPAIR_NOT_POSSIBLE = 2,
        INVALID_ARGUMENTS = 3,
        INSUFFICIENT_DATA = 4,
        REPAIR_FAILED = 5,
        FILE_IO_ERROR = 6,
        LOGIC_ERROR = 7,
        MEMORY_ERROR = 8,
    };

    // Get system RAM and calculate 1/2 of it for memory limit
    // (matches par2cmdline-turbo default behavior)
    static size_t get_memory_limit() {
        size_t total_memory = 0;

#ifdef __APPLE__
        // macOS: use sysctl
        int mib[2] = {CTL_HW, HW_MEMSIZE};
        size_t length = sizeof(total_memory);
        sysctl(mib, 2, &total_memory, &length, NULL, 0);
#elif defined(__linux__)
        // Linux: use sysconf
        long pages = sysconf(_SC_PHYS_PAGES);
        long page_size = sysconf(_SC_PAGE_SIZE);
        if (pages > 0 && page_size > 0) {
            total_memory = (size_t)pages * (size_t)page_size;
        }
#elif defined(_WIN32)
        // Windows: use GlobalMemoryStatusEx
        MEMORYSTATUSEX status;
        status.dwLength = sizeof(status);
        GlobalMemoryStatusEx(&status);
        total_memory = (size_t)status.ullTotalPhys;
#endif

        // Default to 256MB if we can't detect (matches par2cmdline fallback)
        if (total_memory == 0) {
            total_memory = 256 * 1024 * 1024;
        }

        // Use 1/2 of system RAM (matches par2cmdline-turbo default)
        size_t memory_limit = total_memory / 2;

        // Minimum of 16MB and maximum of 2GB
        const size_t MIN_MEMORY = 16 * 1024 * 1024;         // 16MB minimum
        const size_t MAX_MEMORY = 2048ULL * 1024 * 1024;   // 2GB maximum (32-bit safe)

        if (memory_limit < MIN_MEMORY) memory_limit = MIN_MEMORY;
        if (memory_limit > MAX_MEMORY) memory_limit = MAX_MEMORY;

        return memory_limit;
    }

    // Get optimal thread count (matches par2cmdline-turbo behavior)
    static unsigned int get_thread_count() {
        unsigned int hw_threads = std::thread::hardware_concurrency();
        // hardware_concurrency() returns 0 if unable to detect
        return (hw_threads > 0) ? hw_threads : 2; // Fallback to 2 threads
    }

    // Simplified synchronous repair function for Rust FFI
    Par2Result par2_repair_sync(
        const char* parfilename,
        bool do_repair
    ) {
        if (!parfilename) {
            return INVALID_ARGUMENTS;
        }

        std::string par2file(parfilename);

        // Extract directory from par2 file path
        std::string basepath;
        size_t last_slash = par2file.find_last_of("/\\");
        if (last_slash != std::string::npos) {
            basepath = par2file.substr(0, last_slash + 1);
        } else {
            basepath = "./";
        }

        // Collect all non-PAR2 files in the directory to scan for misnamed files
        // This is critical for obfuscated Usenet downloads where filenames don't match
        std::vector<std::string> extrafiles;

#ifndef _WIN32
        DIR *dir = opendir(basepath.c_str());
        if (dir) {
            struct dirent *entry;
            while ((entry = readdir(dir)) != nullptr) {
                std::string filename = entry->d_name;
                // Skip . and .. and PAR2 files
                if (filename != "." && filename != ".." &&
                    filename.find(".par2") == std::string::npos &&
                    filename.find(".PAR2") == std::string::npos &&
                    filename != ".DS_Store") {  // Skip macOS metadata
                    // Use full path for extrafiles
                    extrafiles.push_back(basepath + filename);
                }
            }
            closedir(dir);
        }
#else
        // Windows directory scanning
        WIN32_FIND_DATAA find_data;
        HANDLE hFind = FindFirstFileA((basepath + "*").c_str(), &find_data);
        if (hFind != INVALID_HANDLE_VALUE) {
            do {
                std::string filename = find_data.cFileName;
                if (filename != "." && filename != ".." &&
                    filename.find(".par2") == std::string::npos &&
                    filename.find(".PAR2") == std::string::npos &&
                    !(find_data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY)) {
                    // Use full path for extrafiles
                    extrafiles.push_back(basepath + filename);
                }
            } while (FindNextFileA(hFind, &find_data) != 0);
            FindClose(hFind);
        }
#endif

        // Get adaptive parameters (matches par2cmdline-turbo defaults)
        size_t memory_limit = get_memory_limit();     // 1/2 system RAM
        unsigned int nthreads = get_thread_count();   // Auto-detect CPU cores

        // Suppress PAR2 output by redirecting to /dev/null
        int saved_stdout = -1;
        int saved_stderr = -1;
        int null_fd = -1;

#ifndef _WIN32
        null_fd = open("/dev/null", O_WRONLY);
        if (null_fd != -1) {
            saved_stdout = dup(STDOUT_FILENO);
            saved_stderr = dup(STDERR_FILENO);
            dup2(null_fd, STDOUT_FILENO);
            dup2(null_fd, STDERR_FILENO);
        }
#else
        null_fd = _open("NUL", _O_WRONLY);
        if (null_fd != -1) {
            saved_stdout = _dup(1);
            saved_stderr = _dup(2);
            _dup2(null_fd, 1);
            _dup2(null_fd, 2);
        }
#endif

        // Create dummy output streams that discard output
        std::ostringstream null_out;
        std::ostringstream null_err;

        // Call par2repair with proper parameters
        // CRITICAL: memorylimit must NOT be 0!
        // extrafiles contains all non-PAR2 files in directory for hash-based matching
        Result result = par2repair(
            null_out,                       // stdout (discarded)
            null_err,                       // stderr (discarded)
            nlSilent,                       // noise level (silent)
            memory_limit,                   // memory limit (1/2 system RAM, 16MB-2GB)
            basepath,                       // basepath
            nthreads,                       // nthreads (auto-detected)
            2,                              // filethreads (matches _FILE_THREADS default)
            par2file,                       // PAR2 file path
            extrafiles,                     // extra files to scan for hash matches (misnamed files)
            do_repair,                      // do repair
            false,                          // purge files
            false,                          // skip data
            0                               // skip leaway
        );

        // Restore stdout/stderr
#ifndef _WIN32
        if (saved_stdout != -1) {
            dup2(saved_stdout, STDOUT_FILENO);
            close(saved_stdout);
        }
        if (saved_stderr != -1) {
            dup2(saved_stderr, STDERR_FILENO);
            close(saved_stderr);
        }
        if (null_fd != -1) {
            close(null_fd);
        }
#else
        if (saved_stdout != -1) {
            _dup2(saved_stdout, 1);
            _close(saved_stdout);
        }
        if (saved_stderr != -1) {
            _dup2(saved_stderr, 2);
            _close(saved_stderr);
        }
        if (null_fd != -1) {
            _close(null_fd);
        }
#endif

        // Convert Result to Par2Result
        switch (result) {
            case eSuccess:
                return SUCCESS;
            case eRepairPossible:
                return REPAIR_POSSIBLE;
            case eRepairNotPossible:
                return REPAIR_NOT_POSSIBLE;
            case eInvalidCommandLineArguments:
                return INVALID_ARGUMENTS;
            case eInsufficientCriticalData:
                return INSUFFICIENT_DATA;
            case eRepairFailed:
                return REPAIR_FAILED;
            case eFileIOError:
                return FILE_IO_ERROR;
            case eLogicError:
                return LOGIC_ERROR;
            case eMemoryError:
                return MEMORY_ERROR;
            default:
                return LOGIC_ERROR;
        }
    }
}
