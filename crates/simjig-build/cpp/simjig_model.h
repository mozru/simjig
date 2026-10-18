// simjig_model.h: builds one model's simjig_model_vtable and signal table from a traits struct.
//
// The generator (simjig-build) emits, per model, one shim .cpp that includes the Verilated
// headers and this file, defines a traits struct and exports the vtable:
//
//   #include "Vcounter.h"
//   #include "Vcounter___024root.h"   // only if public internal signals are bound
//   #include "simjig_model.h"
//   struct CounterTraits {
//       using Model = Vcounter;
//       static constexpr const char* kPrefix = "Vcounter";
//       static constexpr uint64_t kIrHash = 0x0123456789abcdefull;
//       static constexpr bool kTiming = false;   // verilated with --timing (coroutines)?
//       static void bind(Vcounter& m, simjig::SignalTable& t) {
//           SIMJIG_SIGNAL(t, m.clk, "clk", 1, SIMJIG_DIR_IN, SIMJIG_ACCESS_READ | SIMJIG_ACCESS_WRITE);
//           SIMJIG_SIGNAL(t, m.rootp->counter__DOT__state, "counter.state", 4,
//                         SIMJIG_DIR_INTERNAL, SIMJIG_ACCESS_READ);
//       }
//   };
//   SIMJIG_EXPORT_VTABLE(Vcounter, CounterTraits)
//
// Invariants:
// - Member addresses are taken once, right after construction, in IR slot order; slot i of the
//   table has id i. Verilator members never move while the model lives.
// - Each SIMJIG_SIGNAL statically checks that the member's C++ type is exactly the storage
//   type Verilator uses for that width (so a width/type drift fails the C++ build, not the
//   simulation).
// - No model code runs outside simjig::guard().
#ifndef SIMJIG_MODEL_H_
#define SIMJIG_MODEL_H_

#include "simjig_abi.h"
#include "simjig_rt.h"

#include <cstddef>
#include <cstdint>
#include <memory>
#include <type_traits>
#include <vector>

namespace simjig {

// Verilator's storage rule (verilated_types.h: CData..QData, VlWide<N>).
constexpr uint8_t storage_for_width(uint32_t width) {
    return width <= 8    ? SIMJIG_STORAGE_C
           : width <= 16 ? SIMJIG_STORAGE_S
           : width <= 32 ? SIMJIG_STORAGE_I
           : width <= 64 ? SIMJIG_STORAGE_Q
                         : SIMJIG_STORAGE_W;
}

constexpr uint32_t words_for_width(uint32_t width) { return (width + 31) / 32; }

constexpr size_t storage_bytes(uint32_t width) {
    return width <= 8    ? 1
           : width <= 16 ? 2
           : width <= 32 ? 4
           : width <= 64 ? 8
                         : 4 * static_cast<size_t>(words_for_width(width));
}

// The storage class of a Verilator member type; undefined for anything else, so binding an
// unexpected type is a compile error.
template <class T>
struct storage_of;
template <>
struct storage_of<CData> : std::integral_constant<uint8_t, SIMJIG_STORAGE_C> {};
template <>
struct storage_of<SData> : std::integral_constant<uint8_t, SIMJIG_STORAGE_S> {};
template <>
struct storage_of<IData> : std::integral_constant<uint8_t, SIMJIG_STORAGE_I> {};
template <>
struct storage_of<QData> : std::integral_constant<uint8_t, SIMJIG_STORAGE_Q> {};
template <std::size_t N>
struct storage_of<VlWide<N>> : std::integral_constant<uint8_t, SIMJIG_STORAGE_W> {};

class SignalTable final {
public:
    void add(uint32_t width, uint8_t dir, uint8_t access, const char* name, void* ptr) {
        const auto id = static_cast<uint32_t>(m_signals.size());
        m_signals.push_back(simjig_signal{id, width, words_for_width(width),
                                          storage_for_width(width), dir, access, 0, name, ptr});
    }
    const std::vector<simjig_signal>& signals() const { return m_signals; }

private:
    std::vector<simjig_signal> m_signals;
};

template <class Traits>
struct ModelHandle final : simjig_handle {
    explicit ModelHandle(const simjig_create_opts* opts) : simjig_handle{opts, Traits::kTiming} {}
    std::unique_ptr<typename Traits::Model> model;
    SignalTable table;
};

template <class Traits>
struct Vtable final {
    using Handle = ModelHandle<Traits>;
    using Model = typename Traits::Model;

    static Model& model(simjig_handle* h) { return *static_cast<Handle*>(h)->model; }

    static simjig_status create(const simjig_create_opts* opts, simjig_handle** out) noexcept {
        if (out == nullptr) return SIMJIG_EXCEPTION;
        *out = nullptr;
        Handle* h = nullptr;
        try {
            h = new Handle{opts};
        } catch (...) {
            return SIMJIG_EXCEPTION;
        }
        *out = h;
        return guard(h, [h, opts] {
            VerilatedContext& ctx = h->rt.context();
            if (opts != nullptr && opts->argc > 0 && opts->argv != nullptr) {
                // commandArgs copies the strings; its non-const parameter is historical.
                ctx.commandArgs(static_cast<int>(opts->argc), const_cast<const char**>(opts->argv));
            }
            if (opts != nullptr && opts->error_limit > 0) {
                ctx.errorLimit(static_cast<int>(opts->error_limit));
            }
            const char* name = opts != nullptr && opts->name != nullptr ? opts->name : "TOP";
            h->model = std::make_unique<Model>(&ctx, name);
            Traits::bind(*h->model, h->table);
        });
    }

    static simjig_status eval(simjig_handle* h) noexcept {
        return guard(h, [h] { model(h).eval(); });
    }

    static simjig_status final_blocks(simjig_handle* h) noexcept {
        return guard(h, [h] { model(h).final(); });
    }

    static uint32_t events_pending(simjig_handle* h) noexcept {
        uint32_t pending = 0;
        if constexpr (Traits::kTiming) {
            guard(h, [h, &pending] { pending = model(h).eventsPending() ? 1 : 0; });
        }
        return pending;
    }

    // Verilator's nextTimeSlot() is a fatal when nothing is pending (and always without
    // --timing); the ABI returns SIMJIG_TIME_NONE instead.
    static uint64_t next_time_slot(simjig_handle* h) noexcept {
        uint64_t next = SIMJIG_TIME_NONE;
        if constexpr (Traits::kTiming) {
            guard(h, [h, &next] {
                if (model(h).eventsPending()) next = model(h).nextTimeSlot();
            });
        }
        return next;
    }

    static const simjig_signal* signals(simjig_handle* h, uint32_t* count) noexcept {
        if (h == nullptr) {
            if (count != nullptr) *count = 0;
            return nullptr;
        }
        const auto& sigs = static_cast<Handle*>(h)->table.signals();
        if (count != nullptr) *count = static_cast<uint32_t>(sigs.size());
        return sigs.data();
    }

    static constexpr simjig_model_vtable table{
        SIMJIG_ABI_VERSION,
        sizeof(simjig_model_vtable),
        Traits::kPrefix,
        VERILATOR_VERSION,
        Traits::kIrHash,
        &create,
        &simjig_rt_destroy,
        &eval,
        &final_blocks,
        &simjig_rt_set_time,
        &events_pending,
        &next_time_slot,
        &signals,
        &simjig_rt_drain_msgs,
        &simjig_rt_trace_open_unsupported,
        &simjig_rt_trace_dump_unsupported,
        &simjig_rt_trace_close_unsupported,
        &simjig_rt_cov_counters_none,
    };
};

}  // namespace simjig

// Binds one member into the signal table, after checking its C++ type against `width`.
#define SIMJIG_SIGNAL(table_, member_, name_, width_, dir_, access_)                            \
    do {                                                                                     \
        using simjig_member_t_ = std::remove_cv_t<std::remove_reference_t<decltype(member_)>>; \
        static_assert(::simjig::storage_of<simjig_member_t_>::value                         \
                          == ::simjig::storage_for_width(width_),                            \
                      "simjig: storage type of " name_ " does not match its width");         \
        static_assert(sizeof(simjig_member_t_) == ::simjig::storage_bytes(width_),          \
                      "simjig: size of " name_ " does not match its width");                 \
        (table_).add((width_), (dir_), (access_), (name_), static_cast<void*>(&(member_)));  \
    } while (0)

// Exports the model's single C symbol, `simjig_vtable_<PREFIX>`.
#define SIMJIG_EXPORT_VTABLE(PREFIX, TRAITS)                                     \
    extern "C" const simjig_model_vtable* simjig_vtable_##PREFIX(void) {         \
        return &::simjig::Vtable<TRAITS>::table;                                 \
    }

#endif  // SIMJIG_MODEL_H_
