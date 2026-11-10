//! Prints every value the generated `origin.kernel` module defines, so tests/generated.rs can
//! compare them with the hand-written `origin-abi`.

use oidl_generated::oidl_rt as rt;
use oidl_generated::origin_kernel as k;
use oidl_generated::origin_kernel::kernel::server::number;

fn main() {
    println!("version {} {}", k::VERSION.0, k::VERSION.1);
    println!("ok {}", k::OK);
    for e in k::Error::ALL {
        println!("status {} {}", e.name(), e.raw());
    }
    println!("status_name {}", k::status_name(k::OK));
    println!("status_name {}", k::status_name(-14));
    println!("status_name {}", k::status_name(k::Error::Limit.raw()));

    let rights = [
        ("READ", k::Rights::READ),
        ("WRITE", k::Rights::WRITE),
        ("MAP", k::Rights::MAP),
        ("EXECUTE", k::Rights::EXECUTE),
        ("DUPLICATE", k::Rights::DUPLICATE),
        ("TRANSFER", k::Rights::TRANSFER),
        ("WAIT", k::Rights::WAIT),
        ("SIGNAL", k::Rights::SIGNAL),
        ("MANAGE", k::Rights::MANAGE),
        ("ALL", k::Rights::ALL),
        ("CHANNEL", k::CHANNEL_RIGHTS),
    ];
    for (name, value) in rights {
        println!("right {name} {}", value.bits());
    }
    let signals = [
        ("READABLE", k::Signals::READABLE),
        ("WRITABLE", k::Signals::WRITABLE),
        ("PEER_CLOSED", k::Signals::PEER_CLOSED),
        ("SIGNALED", k::Signals::SIGNALED),
        ("TERMINATED", k::Signals::TERMINATED),
        ("ALL", k::Signals::ALL),
    ];
    for (name, value) in signals {
        println!("signal {name} {}", value.bits());
    }
    for object in k::ObjectType::ALL {
        println!("object {} {}", object.name(), object.raw());
    }
    let numbers = [
        ("THREAD_EXIT", number::THREAD_EXIT),
        ("DEBUG_REPORT", number::DEBUG_REPORT),
        ("DEBUG_WRITE", number::DEBUG_WRITE),
        ("HANDLE_CLOSE", number::HANDLE_CLOSE),
        ("HANDLE_DUPLICATE", number::HANDLE_DUPLICATE),
        ("CHANNEL_CREATE", number::CHANNEL_CREATE),
        ("CHANNEL_WRITE", number::CHANNEL_WRITE),
        ("CHANNEL_READ", number::CHANNEL_READ),
        ("WAIT_ONE", number::WAIT_ONE),
        ("EVENT_CREATE", number::EVENT_CREATE),
        ("OBJECT_SIGNAL", number::OBJECT_SIGNAL),
    ];
    for (name, value) in numbers {
        println!("number {name} {value}");
    }
    println!("limit MAX_MESSAGE_BYTES {}", k::MAX_MESSAGE_BYTES);
    println!("limit MAX_MESSAGE_HANDLES {}", k::MAX_MESSAGE_HANDLES);
    println!("limit DEADLINE_INFINITE {}", k::DEADLINE_INFINITE);
    println!("read_sizes SIZE {}", k::ReadSizes::SIZE);
    println!("read_sizes SIZE_AT {}", k::ReadSizes::SIZE_AT);
    println!("read_sizes BYTES_AT {}", k::ReadSizes::BYTES_AT);
    println!("read_sizes HANDLES_AT {}", k::ReadSizes::HANDLES_AT);
    println!("read_sizes BASE_SIZE {}", k::ReadSizes::BASE_SIZE);
    println!("read_sizes SENDER_PROCESS_AT {}", k::ReadSizes::SENDER_PROCESS_AT);
    println!("read_sizes SENDER_PRINCIPAL_AT {}", k::ReadSizes::SENDER_PRINCIPAL_AT);
    println!("received_handle SIZE {}", <k::ReceivedHandle as rt::Fixed>::SIZE);
    println!("received_handle HANDLE_AT {}", k::ReceivedHandle::HANDLE_AT);
    println!("received_handle OBJECT_TYPE_AT {}", k::ReceivedHandle::OBJECT_TYPE_AT);
    println!("received_handle RIGHTS_AT {}", k::ReceivedHandle::RIGHTS_AT);
    println!("handle INVALID {}", rt::Handle::INVALID.to_abi());
    for raw in 0..=255u8 {
        if let Some(value) = rt::decode_bool(raw) {
            println!("bool {raw} {value}");
        }
    }
}
