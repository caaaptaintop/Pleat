#include <libproc.h>
#include <netinet/in.h>
#include <stdint.h>
#include <string.h>
#include <sys/proc_info.h>
#include <sys/socket.h>

#define PLEAT_PEER_MAX_FDS 512

struct peer_endpoint {
    unsigned char address[16];
    uint16_t port; /* Network byte order, including libproc's int port fields. */
};

static void peer_address(unsigned char out[16], const void *address, int ipv4) {
    memset(out, 0, 16);
    if (ipv4) {
        out[10] = out[11] = 0xff;
        memcpy(out + 12, address, 4);
    } else {
        memcpy(out, address, 16);
    }
}

static int peer_sockaddr(const struct sockaddr_storage *ss, socklen_t size,
                         struct peer_endpoint *out) {
    if (ss->ss_family == AF_INET && size == sizeof(struct sockaddr_in) &&
        ss->ss_len == size) {
        const struct sockaddr_in *a = (const struct sockaddr_in *)ss;
        peer_address(out->address, &a->sin_addr, 1);
        out->port = a->sin_port;
    } else if (ss->ss_family == AF_INET6 && size == sizeof(struct sockaddr_in6) &&
               ss->ss_len == size) {
        const struct sockaddr_in6 *a = (const struct sockaddr_in6 *)ss;
        /* Scoped addresses are outside the native loopback transport contract. */
        if (a->sin6_scope_id != 0) return -1;
        peer_address(out->address, &a->sin6_addr, 0);
        out->port = a->sin6_port;
    } else {
        return -1;
    }
    return out->port != 0 ? 0 : -1;
}

static int peer_client(int fd, struct peer_endpoint *local,
                       struct peer_endpoint *remote) {
    struct sockaddr_storage l = {0}, r = {0};
    socklen_t ll = sizeof(l), rl = sizeof(r), tl = sizeof(int);
    int type = 0;
    if (getsockopt(fd, SOL_SOCKET, SO_TYPE, &type, &tl) != 0 ||
        tl != sizeof(type) || type != SOCK_STREAM ||
        getsockname(fd, (struct sockaddr *)&l, &ll) != 0 ||
        getpeername(fd, (struct sockaddr *)&r, &rl) != 0) return -1;
    return peer_sockaddr(&l, ll, local) || peer_sockaddr(&r, rl, remote) ? -1 : 0;
}

static int peer_equal(const struct peer_endpoint *a, const struct peer_endpoint *b) {
    return a->port == b->port && memcmp(a->address, b->address, 16) == 0;
}

static int peer_matches(const struct socket_info *s,
                        const struct peer_endpoint *local,
                        const struct peer_endpoint *remote) {
    const struct tcp_sockinfo *tcp = &s->soi_proto.pri_tcp;
    const struct in_sockinfo *ip = &tcp->tcpsi_ini;
    struct peer_endpoint l, r;
    if (s->soi_kind != SOCKINFO_TCP || s->soi_protocol != IPPROTO_TCP ||
        s->soi_type != SOCK_STREAM || tcp->tcpsi_state != TSI_S_ESTABLISHED) return 0;
    if (s->soi_so == 0 || s->soi_pcb == 0 || ip->insi_gencnt == 0 ||
        !(s->soi_state & SOI_S_ISCONNECTED) ||
        (s->soi_state & (SOI_S_NOFDREF | SOI_S_INCOMP | SOI_S_COMP |
                         SOI_S_ISDISCONNECTING | SOI_S_ISDISCONNECTED)) ||
        (s->soi_family != AF_INET && s->soi_family != AF_INET6) ||
        (ip->insi_vflag != INI_IPV4 && ip->insi_vflag != INI_IPV6) ||
        (s->soi_family == AF_INET && ip->insi_vflag != INI_IPV4) ||
        ip->insi_lport <= 0 || ip->insi_lport > UINT16_MAX ||
        ip->insi_fport <= 0 || ip->insi_fport > UINT16_MAX) return -1;
    /* AF_INET6 may carry INI_IPV4 for a dual-stack accepted IPv4 socket. */
    int ipv4 = ip->insi_vflag == INI_IPV4;
    peer_address(l.address, ipv4 ? (const void *)&ip->insi_laddr.ina_46.i46a_addr4
                                : (const void *)&ip->insi_laddr.ina_6, ipv4);
    peer_address(r.address, ipv4 ? (const void *)&ip->insi_faddr.ina_46.i46a_addr4
                                : (const void *)&ip->insi_faddr.ina_6, ipv4);
    l.port = (uint16_t)ip->insi_lport;
    r.port = (uint16_t)ip->insi_fport;
    return peer_equal(&l, remote) && peer_equal(&r, local);
}

/* Read-only: 1 = proven accepted peer, 0 = no match, -1 = unknown/error.
 * Caller must retain this same connected client stream through authentication
 * and revalidate the process incarnation before sending any credentials. */
int pleat_copilot_peer_owned(int pid, int client_fd) {
    struct proc_fdinfo fds[PLEAT_PEER_MAX_FDS];
    struct peer_endpoint local, remote;
    int unknown = 0;
    if (pid <= 0 || client_fd < 0 || peer_client(client_fd, &local, &remote) != 0)
        return -1;
    int bytes = proc_pidinfo(pid, PROC_PIDLISTFDS, 0, fds, sizeof(fds));
    /* A full buffer cannot distinguish an exact fit from truncation. */
    if (bytes <= 0 || bytes >= (int)sizeof(fds) || bytes % sizeof(fds[0]) != 0)
        return -1;
    for (int i = 0; i < bytes / (int)sizeof(fds[0]); ++i) {
        if (fds[i].proc_fd < 0) { unknown = 1; continue; }
        if (fds[i].proc_fdtype != PROX_FDTYPE_SOCKET) {
            switch (fds[i].proc_fdtype) {
                case PROX_FDTYPE_ATALK: case PROX_FDTYPE_VNODE:
                case PROX_FDTYPE_PSHM: case PROX_FDTYPE_PSEM:
                case PROX_FDTYPE_KQUEUE: case PROX_FDTYPE_PIPE:
                case PROX_FDTYPE_FSEVENTS: case PROX_FDTYPE_NETPOLICY:
                case PROX_FDTYPE_CHANNEL: case PROX_FDTYPE_NEXUS: break;
                default: unknown = 1;
            }
            continue;
        }
        struct socket_fdinfo first = {0}, again = {0};
        if (proc_pidfdinfo(pid, fds[i].proc_fd, PROC_PIDFDSOCKETINFO,
                           &first, sizeof(first)) != sizeof(first)) {
            unknown = 1;
            continue;
        }
        int match = peer_matches(&first.psi, &local, &remote);
        if (match < 0) unknown = 1;
        if (match != 1) continue;
        if (proc_pidfdinfo(pid, fds[i].proc_fd, PROC_PIDFDSOCKETINFO,
                           &again, sizeof(again)) != sizeof(again) ||
            peer_matches(&again.psi, &local, &remote) != 1 ||
            first.psi.soi_so != again.psi.soi_so ||
            first.psi.soi_pcb != again.psi.soi_pcb ||
            first.psi.soi_proto.pri_tcp.tcpsi_tp != again.psi.soi_proto.pri_tcp.tcpsi_tp ||
            first.psi.soi_proto.pri_tcp.tcpsi_ini.insi_gencnt !=
                again.psi.soi_proto.pri_tcp.tcpsi_ini.insi_gencnt) return -1;
        struct peer_endpoint final_local, final_remote;
        if (peer_client(client_fd, &final_local, &final_remote) != 0 ||
            !peer_equal(&local, &final_local) || !peer_equal(&remote, &final_remote))
            return -1;
        return 1;
    }
    return unknown ? -1 : 0;
}
